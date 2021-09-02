mod tracker;
pub mod peer;
pub mod file;
pub mod piece;
pub mod utils;

use std::sync::Arc;

use tokio::sync::RwLock;
use rusty_parser::ParsedTorrent;

use file::File;
use piece::Piece;
use peer::{Bitfield, Peer};
use tracker::{TrackerKind, TrackerEvent};

pub struct TorrentInfo {
    data: ParsedTorrent,
    piece_length: usize,

    torrent_files: Arc<RwLock<Vec<File>>>,
    torrent_pieces: Arc<RwLock<Vec<Piece>>>,

    pieces_hashes: Arc<Vec<Vec<u8>>>,
    pieces_missing: Arc<RwLock<Vec<usize>>>,

    bitfield_client: Arc<RwLock<Bitfield>>
}

pub struct Engine {
    trackers: Vec<TrackerKind>,
    torrent_info: Arc<TorrentInfo>
}

impl Engine {
    pub async fn init(data: Vec<u8>) -> Engine {
        let data = ParsedTorrent::new(data);
        let piece_length = data.info().piece_length() as usize;

        let trackers_list = {
            let mut list = vec![data.announce().clone()];
            let announce_list = data.announce_list();

            for item in announce_list {
                if !item.is_empty() {
                    list.push(item.clone());
                }
            }

            list
        };
        
        let torrent_files = Engine::build_file_list(data.info().files(), piece_length).await;
        let torrent_files = Arc::new(RwLock::new(torrent_files));

        println!("Torrent name: {}", data.get_name());

        let pieces_hashes = data.info().pieces().to_vec();
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
        let mut peers: Vec<Box<dyn Peer+Send>> = Vec::new();

        for tracker in self.trackers.iter_mut() {
            match tracker {
                TrackerKind::Tcp(t) => {
                    t.send_message(TrackerEvent::Started).await;
                    peers.append(&mut t.get_peers());
                },
                TrackerKind::Udp => unimplemented!(),
            }
        }

        if peers.is_empty() {
            println!("No peers received from trackers, exiting...");
        }

        for peer in peers {
            tokio::task::spawn(async move {
                let mut peer = peer;

                peer.connect().await;
                peer.handle_events().await;
            });
        }

        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    async fn build_file_list(files_data: &[(std::string::String, u64)], piece_len: usize) -> Vec<File> {
        let mut list = Vec::new();

        for (filename, size) in files_data {
            list.push(File::new(filename.to_string(), *size as usize, piece_len).await);
        }

        list
    }
}
