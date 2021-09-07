use std::sync::Arc;
use std::time::{Duration, Instant};
use std::net::{Ipv4Addr, SocketAddrV4};

use bytes::Buf;
use tokio::sync::RwLock;
use rusty_parser::BEncodeType;

use crate::engine::TorrentInfo;
use crate::engine::peer::{Peer, PeerInfo};
use crate::engine::peer::tcp::TcpPeer;
use crate::engine::tracker::TrackerEvent;

pub struct TcpTracker {
    client_id: String,
    tracker_url: String,

    info: Arc<TorrentInfo>,
    peers_list: Vec<SocketAddrV4>,

    announced: bool,
    announce_interval: Duration,
    time_since_last_message: Instant
}

impl TcpTracker {
    pub fn new(client_id: String, tracker_url: String, info: Arc<TorrentInfo>) -> TcpTracker {
        if tracker_url.starts_with("udp:") {
            panic!("Got an udp tracker url on TcpTracker");
        }

        TcpTracker {
            client_id,
            tracker_url,

            info,
            peers_list: Vec::new(),

            announced: false,
            announce_interval: Duration::from_secs(20),
            time_since_last_message: Instant::now()
        }
    }

    pub async fn get_peers(&self) -> Vec<Box<dyn Peer+Send>> {
        let mut peers: Vec<Box<dyn Peer+Send>> = Vec::new();

        for address in self.peers_list.iter() {
            let peer_info = Arc::new(RwLock::new(PeerInfo::new(*address)));

            peers.push(Box::new(TcpPeer::new(*address, peer_info, self.info.clone()).await));
        }

        peers
    }

    pub async fn send_message(&mut self, event: TrackerEvent) {
        if let TrackerEvent::PeriodicRequest = event {
            if self.announced && self.time_since_last_message.elapsed() < self.announce_interval {
                return;
            }
        }

        let event = match event {
            TrackerEvent::Started => String::from("&event=started"),
            TrackerEvent::Stopped => String::from("&event=stopped"),
            TrackerEvent::Completed => String::from("&event=completed"),
            TrackerEvent::PeriodicRequest => String::new()
        };

        let info = self.info.data.info();
        let info_hash = urlencoding::encode_binary(info.info_hash());

        let piece_size = self.info.piece_length;
        let total_size = piece_size * self.info.torrent_pieces.read().await.len();

        // This doesn't consider a smaller final piece, but I don't think it *really* matters.
        let missing = self.info.piece_length * self.info.get_missing_pieces_count().unwrap_or(self.info.get_pieces_count().await);
        let downloaded = total_size - missing;

        let tracker_query = format!(
            "{}?info_hash={}&peer_id={}&port=6881&uploaded={}&downloaded={}&left={}&compact=1{}",
            self.tracker_url, info_hash, self.client_id, 0, downloaded, missing, event
        );
        
        for _ in 0..3 {
            let response = reqwest::get(&tracker_query).await;

            if let Ok(response) = response {
                let body: Vec<u8> = response.bytes().await.unwrap_or_default().to_vec();
                let response_data = BEncodeType::dictionary(&body, &mut 1);
                
                let entries = response_data.get_dictionary();

                if let Some(peers) = entries.get("peers") {
                    let peers_bytes = peers.get_string_bytes();
                    let mut peers_bytes_slice = peers_bytes.as_slice();

                    for _ in (0..peers_bytes.len()).step_by(6) {
                        let ip = peers_bytes_slice.get_u32();
                        let port = peers_bytes_slice.get_u16();
                        let socket_addr = SocketAddrV4::new(Ipv4Addr::from(ip), port);

                        if !self.peers_list.contains(&socket_addr) {
                            self.peers_list.push(socket_addr);
                        }
                    }
                }

                if let Some(interval) = entries.get("interval") {
                    self.announce_interval = Duration::from_secs(interval.get_int() as u64);
                }

                self.announced = true;
                break;
            }
        }
    }
}
