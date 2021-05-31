mod tracker;
pub mod peer;
pub mod file;
pub mod piece;
pub mod utils;

use std::sync::Arc;

use tokio::sync::RwLock;
use rusty_parser::Torrent;

use file::File;
use peer::{Bitfield, Peer};
use tracker::{Tracker, TrackerEvent};

use crate::types::*;

pub struct TorrentInfo {
    data: Torrent,
    piece_length: usize,

    torrent_files: TFiles,
    torrent_pieces: TPieces,

    pieces_hashes: THashes,
    pieces_missing: TMissing,

    bitfield_client: TClientBitfield
}

pub struct Engine {
    trackers: Vec<Box<dyn Tracker>>,
    torrent_info: Arc<TorrentInfo>
}

impl Engine {
    pub async fn init(data: Vec<u8>) -> Engine {
        let data = Torrent::new(data).unwrap();
        let (info, _) = data.info();
        let piece_length = info.get("piece length").unwrap().get_int() as usize;

        let trackers_list = {
            let mut list = vec![data.announce()];
            let announce_list = data.announce_list().get_list();

            for item in announce_list {
                let item = item.get_string();

                if !item.is_empty() {
                    list.push(item);
                }
            }

            list
        };
        
        let torrent_files = Engine::build_file_list(data.get_files(), piece_length as usize).await;
        let torrent_files = Arc::new(RwLock::new(torrent_files));

        println!("Torrent name: {}", info.get("name").unwrap().get_string());

        let pieces_hashes = Engine::build_hashes_list(info.get("pieces").unwrap().get_string_bytes()).await;
        let pieces = pieces_hashes.len();
        let pieces_hashes = Arc::from(pieces_hashes);

        let torrent_info = TorrentInfo {
            data,
            piece_length,

            torrent_files,
            torrent_pieces: Arc::new(RwLock::new(Vec::new())),

            pieces_hashes,
            pieces_missing: Arc::new(RwLock::new(Vec::new())),

            bitfield_client: Arc::new(RwLock::new(Bitfield::empty(pieces)))
        };

        let mut torrent_info = Arc::new(torrent_info);

        utils::check_torrent(&mut torrent_info).await;
        utils::update_missing_pieces(&mut torrent_info).await;

        let trackers = tracker::create_trackers(trackers_list, torrent_info.clone());

        Engine {
            trackers,
            torrent_info
        }
    }

    pub async fn start_torrent(mut self) {
        let mut peers: Vec<Box<dyn Peer + Send>> = Vec::new();
        let mut tasks = Vec::new();

        for tracker in self.trackers.iter_mut() {
            tracker.send_message(TrackerEvent::Started).await;
            let tracker_peers = tracker.get_peers();

            for address in tracker_peers {
                // TODO: Figure out how to handle UDP peers.
                peers.push(Box::new(peer::tcp::TcpPeer::new(*address, self.torrent_info.clone())));
            }
        }

        if peers.is_empty() {
            println!("No peers received from trackers, exiting...");
        }

        for peer in peers {
            let handle = tokio::task::spawn(async move {
                let mut peer = peer;

                peer.handle_events().await;
            });

            tasks.push(handle);
        }

        for handle in tasks {
            handle.await.unwrap();
        }
    }

    async fn build_file_list(files_data: Vec<(String, i64)>, piece_len: usize) -> Vec<File> {
        let mut list = Vec::new();

        for (filename, size) in files_data {
            list.push(File::new(filename, size as usize, piece_len).await);
        }

        list
    }

    async fn build_hashes_list(data: Vec<u8>) -> Vec<Vec<u8>> {
        let mut hashes = Vec::new();

        for chunk in data.chunks(20) {
            hashes.push(chunk.to_owned());
        }

        hashes
    }
}
