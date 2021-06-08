pub mod tcp;

use async_trait::async_trait;

use crate::types::*;
use crate::engine::peer::Peer;

#[async_trait]
pub trait Tracker {
    fn get_peers(&self) -> Vec<Box<dyn Peer+Send>>;
    async fn send_message(&mut self, event: TrackerEvent);
}

#[derive(PartialEq)]
pub enum TrackerEvent {
    Started,
    Stopped,
    Completed,

    PeriodicRequest
}

pub fn create_trackers(trackers: Vec<String>, info: TInfo) -> Vec<Box<dyn Tracker>> {
    let mut result: Vec<Box<dyn Tracker>> = Vec::new();

    for tracker in trackers {
        if tracker.starts_with("udp:") {
            println!("UDP tracker found, ignoring... ({})", tracker);
        }
        else {
            // TODO: bruh
            let client_id: String = vec!['r'; 20].iter().collect();
            let tracker = tcp::TcpTracker::new(client_id, tracker, info.clone());

            result.push(Box::new(tracker));
        }
    }

    result
}
