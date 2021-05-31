use std::time::{Duration, Instant};
use std::net::{Ipv4Addr, SocketAddrV4};

use bytes::Buf;
use async_trait::async_trait;
use rusty_parser::BEncodeType;

use crate::types::*;
use crate::engine::peer::tcp::TcpPeer;
use crate::engine::tracker::{Tracker, TrackerEvent};

pub struct TcpTracker {
    client_id: String,
    tracker_url: String,

    info: TInfo,
    peers_list: Vec<SocketAddrV4>,

    announced: bool,
    announce_interval: Duration,
    time_since_last_message: Instant
}

impl TcpTracker {
    pub fn new(client_id: String, tracker_url: String, info: TInfo) -> TcpTracker {
        if tracker_url.starts_with("udp:") {
            panic!("Got an udp tracker url on  TcpTracker");
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
}

#[async_trait]
impl Tracker for TcpTracker {
    fn get_peers(&self) -> &Vec<SocketAddrV4> {
        &self.peers_list
    }

    async fn send_message(&mut self, event: TrackerEvent) {
        let should_reannounce = self.time_since_last_message.elapsed() < self.announce_interval;

        if self.announced && event == TrackerEvent::PeriodicRequest && !should_reannounce {
            return;
        }

        println!("Attempting announce to {}", self.tracker_url);

        let event = match event {
            TrackerEvent::Started => String::from("&event=started"),
            TrackerEvent::Stopped => String::from("&event=stopped"),
            TrackerEvent::Completed => String::from("&event=completed"),
            TrackerEvent::PeriodicRequest => String::new()
        };

        let info = self.info.data.info();
        let info_hash = urlencoding::encode_binary(&info.1);

        // This doesn't consider a smaller final piece, but I don't think it *really* matters.
        let missing_data = self.info.piece_length * self.info.pieces_missing.read().await.len();

        let mut tracker_url = format!(
            "{}?info_hash={}&peer_id={}&port=6881&uploaded={}&downloaded={}&left={}&compact=1",
            self.tracker_url, info_hash, self.client_id, 0, 0, missing_data
        );

        if !event.is_empty() {
            tracker_url.push_str(&event);
        }
        
        for attempt in 0..3 {
            let response = reqwest::get(&tracker_url).await;

            if let Ok(response) = response {
                let body: Vec<u8> = response.bytes().await.unwrap_or_default().to_vec();
                
                if let Ok(response_data) = BEncodeType::dictionary(&body, &mut 1) {
                    let entries = response_data.get_dictionary();

                    if let Some(peers) = entries.get("peers") {
                        let mut peers_list = Vec::new();
                        let peers_bytes = peers.get_string_bytes();
                        let mut peers_bytes_slice = peers_bytes.as_slice();

                        for _ in (0..peers_bytes.len()).step_by(6) {
                            let ip = peers_bytes_slice.get_u32();
                            let port = peers_bytes_slice.get_u16();

                            peers_list.push(SocketAddrV4::new(Ipv4Addr::from(ip), port));
                        }

                        self.peers_list = peers_list;
                    }
                    else if let Some(reason) = entries.get("failure reason") {
                        let reason = reason.get_string();

                        if !reason.is_empty() {
                            println!("Failed to announce on tracker: {}", reason);
                        }
                    }

                    if let Some(interval) = entries.get("interval") {
                        self.announce_interval = Duration::from_secs(interval.get_int() as u64);
                    }

                    self.announced = true;
                    println!("Announced successfully at {}. Got {} peers.", self.tracker_url, self.peers_list.len());
                }
                else {
                    println!("Couldn't parse the trackers response. Data received: {}", String::from_utf8_lossy(&body));
                }

                break;
            }
            else if let Err(e) = response {
                if let Some(code) = e.status() {
                    println!(
                        "Error sending message to tracker {} (code {}). Retrying... ({}/5)",
                        self.tracker_url,
                        code,
                        attempt + 1
                    );
                }
                else {
                    println!(
                        "Error sending message to tracker {}. Retrying... ({}/5)",
                        self.tracker_url,
                        attempt + 1
                    );
                }
            }
        }
    }
}
