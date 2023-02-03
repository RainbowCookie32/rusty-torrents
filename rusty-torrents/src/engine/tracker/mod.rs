pub mod tcp;

use std::sync::Arc;

use crate::engine::TorrentInfo;
use crate::engine::tracker::tcp::TcpTracker;

pub enum TrackerKind {
    Tcp(TcpTracker),
    Udp
}

pub enum TrackerEvent {
    Started,
    Stopped,
    Completed,

    PeriodicRequest
}

pub fn create_trackers(trackers: Vec<String>, info: Arc<TorrentInfo>) -> Vec<TrackerKind> {
    let mut result = Vec::new();

    for tracker in trackers {
        if tracker.starts_with("udp:") {
            // println!("UDP tracker found, ignoring... ({})", tracker);
        }
        else {
            // TODO: bruh
            let client_id: String = vec!['r'; 20].iter().collect();
            let tracker = TcpTracker::new(client_id, tracker, info.clone());

            result.push(TrackerKind::Tcp(tracker));
        }
    }

    result
}
