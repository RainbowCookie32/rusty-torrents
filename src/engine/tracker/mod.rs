use std::sync::Arc;
use std::time::{Duration, Instant};
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6, Ipv4Addr, Ipv6Addr};

use bytes::{Buf, Bytes};
use reqwest::Client;

use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio::net::UdpSocket;

use crate::bencode::BEncodeType;
use crate::engine::transfer::TransferProgress;

type PeersTx = mpsc::Sender<Vec<SocketAddr>>;
type ProgressRx = broadcast::Receiver<TransferProgress>;

struct Tracker {
    url: String,
    kind: TrackerKind,

    peer_id: String,
    connection_id: String,

    complete_announced: bool,

    error_count: u32,
    reannounce_rate: Option<Duration>,
    last_announce_time: Option<Instant>
}

impl Tracker {
    pub fn new(url: String, peer_id: String) -> Tracker {
        let (url, kind) = {
            if url.starts_with("udp") {
                let url = url.split('/').collect::<Vec<&str>>();
                (url[2].to_owned(), TrackerKind::Udp)
            }
            else {
                (url, TrackerKind::Tcp)
            }
        };
        
        Tracker {
            url,
            kind,

            peer_id,
            connection_id: String::new(),

            complete_announced: false,

            error_count: 0,
            reannounce_rate: None,
            last_announce_time: None
        }
    }
}

enum TrackerKind {
    Tcp,
    Udp
}

pub struct TrackersHandler {
    tcp_client: Client,
    udp_socket: UdpSocket,

    info_hash: Arc<[u8; 20]>,
    progress: TransferProgress,

    trackers: Vec<Tracker>,

    new_peers_tx: mpsc::Sender<Vec<SocketAddr>>,
    transfer_progress_rx: broadcast::Receiver<TransferProgress>
}

impl TrackersHandler {
    pub async fn init(info_hash: Arc<[u8; 20]>, progress: TransferProgress, trackers: Vec<String>, peers_tx: PeersTx, progress_rx: ProgressRx) -> TrackersHandler {
        let peer_id: String = vec!['0'; 20].iter().collect();

        let trackers = trackers.into_iter()
            .map(| url | Tracker::new(url, peer_id.clone()))
            .collect()
        ;

        TrackersHandler {
            tcp_client: Client::new(),
            udp_socket: UdpSocket::bind("0.0.0.0:0").await.unwrap(),

            info_hash,
            progress,

            trackers,

            new_peers_tx: peers_tx,
            transfer_progress_rx: progress_rx
        }
    }

    pub async fn start(mut self) {
        loop {
            let mut received_peers = Vec::new();

            if let Some(mut peers) = self.announce_tcp_trackers().await {
                received_peers.append(&mut peers);
            }
            
            if let Some(mut peers) = self.announce_udp_trackers().await {
                received_peers.append(&mut peers);
            }

            received_peers.sort_unstable();
            received_peers.dedup();

            if !received_peers.is_empty() {
                self.new_peers_tx.send(received_peers).await
                    .expect("failed to send peers info to engine");
            }

            if let Ok(progress) = self.transfer_progress_rx.try_recv() {
                self.progress = progress;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn announce_tcp_trackers(&mut self) -> Option<Vec<SocketAddr>> {
        let hash = urlencoding::encode_binary(self.info_hash.as_ref());
        let mut peers_list = Vec::new();

        for tracker in self.trackers.iter_mut() {
            if let Some(last_announce) = tracker.last_announce_time.as_ref() {
                // If we announce successfully, we should have the interval too.
                let announce_interval = tracker.reannounce_rate.as_ref().unwrap();

                if &last_announce.elapsed() < announce_interval {
                    continue;
                }
            }

            let event = {
                if self.progress.left == 0 && !tracker.complete_announced {
                    "completed"
                }
                else if tracker.last_announce_time.is_none() {
                    "started"
                }
                else {
                    ""
                }
            };
    
            let query_url = format!(
                "{}?info_hash={}&peer_id={}&port=6881&uploaded={}&downloaded={}&left={}&compact=1&numwant=100&event={}",
                tracker.url, hash, &tracker.peer_id, self.progress.uploaded, self.progress.downloaded, self.progress.left, event
            );
    
            let request = self.tcp_client.get(&query_url);

            let response = {
                if let Ok(response) = request.send().await {
                    response
                }
                else {
                    continue;
                }
            };

            let body_bytes = {
                if let Ok(body_bytes) = response.bytes().await {
                    body_bytes
                }
                else {
                    continue;
                }
            };
    
            let response_dictionary = BEncodeType::dictionary(&body_bytes, &mut 1);
            let response_dictionary_hm = response_dictionary.get_dictionary();
    
            if let Some(peers_v4) = response_dictionary_hm.get("peers") {
                let peers_bytes = peers_v4.get_string_bytes();
                let mut peers_slice = Bytes::from(peers_bytes);

                while peers_slice.has_remaining() {
                    let ip = peers_slice.get_u32();
                    let port = peers_slice.get_u16();
                    let addr = SocketAddr::V4(
                        SocketAddrV4::new(
                            Ipv4Addr::from(ip),
                            port
                        )
                    );
    
                    peers_list.push(addr);
                }
            }
    
            if let Some(peers_v6) = response_dictionary_hm.get("peers6") {
                let peers_bytes = peers_v6.get_string_bytes();
                let mut peers_slice = Bytes::from(peers_bytes);

                while peers_slice.has_remaining() {
                    let ip = peers_slice.get_u128();
                    let port = peers_slice.get_u16();
                    let addr = SocketAddr::V6(
                        SocketAddrV6::new(
                            Ipv6Addr::from(ip),
                            port,
                            0,
                            0
                        )
                    );
    
                    peers_list.push(addr);
                }
            }
    
            if let Some(interval) = response_dictionary_hm.get("interval") {
                // Reannounce rates can be a bit on the high side, and sometimes we can end up with slow peers.
                // Trackers *usually* don't seem to mind going below their specified rate, as long as they don't get spammed.
                tracker.reannounce_rate = Some(Duration::from_secs((interval.get_int() as u64).min(120)));
            }
    
            if event == "completed" {
                tracker.complete_announced = true;
            }
            
            tracker.last_announce_time = Some(Instant::now());
        }
        
        Some(peers_list)
    }

    async fn announce_udp_trackers(&mut self) -> Option<Vec<SocketAddr>> {
        None
    }
}
