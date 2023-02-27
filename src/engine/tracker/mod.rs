use std::sync::Arc;
use std::time::{Duration, Instant};
use std::net::{SocketAddrV4, Ipv4Addr};

use bytes::Buf;
use reqwest::Client;

use tokio::sync::mpsc;
use tokio::sync::broadcast;
use tokio::net::UdpSocket;

use crate::bencode::BEncodeType;
use crate::engine::transfer::TransferProgress;

type PeersTx = mpsc::Sender<Vec<SocketAddrV4>>;
type ProgressRx = broadcast::Receiver<TransferProgress>;

enum TrackerKind {
    Tcp,
    Udp { connection_id: String }
}

pub struct TrackersHandler {
    info_hash: Arc<[u8; 20]>,
    progress: TransferProgress,

    trackers: Vec<Tracker>,

    new_peers_tx: mpsc::Sender<Vec<SocketAddrV4>>,
    transfer_progress_rx: broadcast::Receiver<TransferProgress>
}

impl TrackersHandler {
    pub fn init(info_hash: Arc<[u8; 20]>, progress: TransferProgress, trackers: Vec<String>, peers_tx: PeersTx, progress_rx: ProgressRx) -> TrackersHandler {
        let peer_id: String = vec!['0'; 20].iter().collect();

        let trackers = trackers.into_iter()
            .map(| url | Tracker::new(url, peer_id.clone()))
            .collect()
        ;

        TrackersHandler {
            info_hash,
            progress,

            trackers,

            new_peers_tx: peers_tx,
            transfer_progress_rx: progress_rx
        }
    }

    pub async fn start(mut self) {
        let tcp_client = Client::new();
        let _udp_socket = UdpSocket::bind("0.0.0.0:0").await.expect("failed to bind udp socket");

        loop {
            let mut received_peers = Vec::new();

            for tracker in self.trackers.iter_mut() {
                let (should_announce, reannounce) = {
                    // reannounce_rate should be set after a successful announce.
                    if let Some(reannounce_rate) = tracker.reannounce_rate.as_ref() {
                        if let Some(last_announce) = tracker.last_announce_time.as_ref() {
                            (&last_announce.elapsed() > reannounce_rate, true)
                        }
                        else {
                            (true, true)
                        }
                    }
                    else {
                        (true, false)
                    }
                };

                if should_announce {
                    match tracker.kind {
                        TrackerKind::Tcp => {
                            if let Some(mut peers) = tracker.announce_tcp(&tcp_client, &self.info_hash, &self.progress, reannounce).await {
                                received_peers.append(&mut peers);
                            }
                            else {
                                tracker.error_count += 1;
                            }
                        }
                        TrackerKind::Udp { .. } => {
                            // TODO: soon(tm)
                        }
                    }
                }

                tokio::task::yield_now().await;
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
}

struct Tracker {
    url: String,
    kind: TrackerKind,

    peer_id: String,
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
                (url[2].to_owned(), TrackerKind::Udp { connection_id: String::new() })
            }
            else {
                (url, TrackerKind::Tcp)
            }
        };
        
        Tracker {
            url,
            kind,

            peer_id,
            complete_announced: false,

            error_count: 0,
            reannounce_rate: None,
            last_announce_time: None
        }
    }

    pub async fn announce_tcp(&mut self, client: &Client, hash: &[u8; 20], progress: &TransferProgress, reannounce: bool) -> Option<Vec<SocketAddrV4>> {
        let hash = urlencoding::encode_binary(hash);
        let event = {
            if progress.left == 0 && !self.complete_announced {
                "completed"
            }
            else if !reannounce {
                "started"
            }
            else {
                ""
            }
        };

        let query_url = format!(
            "{}?info_hash={}&peer_id={}&port=6881&uploaded={}&downloaded={}&left={}&compact=1&numwant=100&event={}",
            self.url, hash, &self.peer_id, progress.uploaded, progress.downloaded, progress.left, event
        );

        let request = client.get(&query_url);

        let response = request.send().await.ok()?;
        let body_bytes = response.bytes().await.ok()?.to_vec();

        let response_dictionary = BEncodeType::dictionary(&body_bytes, &mut 1);
        let response_dictionary_hm = response_dictionary.get_dictionary();

        let peers = response_dictionary_hm.get("peers")?;
        let peers_bytes = peers.get_string_bytes();
        let mut peers_slice = peers_bytes.as_slice();

        let mut peers_list = Vec::with_capacity(peers_bytes.len() / 6);

        for _ in (0..peers_bytes.len()).step_by(6) {
            let ip = peers_slice.get_u32();
            let port = peers_slice.get_u16();
            let addr = SocketAddrV4::new(Ipv4Addr::from(ip), port);

            peers_list.push(addr);
        }

        if let Some(interval) = response_dictionary_hm.get("interval") {
            // Reannounce rates can be a bit on the high side, and sometimes we can end up with slow peers.
            // Trackers *usually* don't seem to mind going below their specified rate, as long as they don't get spammed.
            self.reannounce_rate = Some(Duration::from_secs(interval.get_int().min(120)));
        }

        if event == "completed" {
            self.complete_announced = true;
        }
        
        self.last_announce_time = Some(Instant::now());

        Some(peers_list)
    }
}
