mod peer;
mod file;
mod piece;
mod utils;
mod tracker;

use std::sync::Arc;

use rand::Rng;
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

    bitfield_client: Arc<RwLock<Bitfield>>
}

impl TorrentInfo {
    pub async fn request_piece(&self, idx: usize) {
        if let Some(piece) = self.torrent_pieces.write().await.get_mut(idx) {
            piece.set_requested(true);
        }
    }

    pub async fn get_unfinished_piece_idx(&self) -> usize {
        let pieces = self.torrent_pieces.read().await;
        rand::thread_rng().gen_range(0..pieces.iter().filter(|p| !p.finished() && !p.requested()).count())
    }

    pub async fn get_pieces_count(&self) -> usize {
        self.torrent_pieces.read().await.len()
    }

    pub async fn get_missing_pieces_count(&self) -> usize {
        let pieces = self.torrent_pieces.read().await;
        pieces.iter().filter(|p| !p.finished()).count()
    }
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

        let pieces = data.info().pieces().len();

        let torrent_info = TorrentInfo {
            data,
            piece_length,

            torrent_files,
            torrent_pieces: Arc::new(RwLock::new(Vec::new())),

            bitfield_client: Arc::new(RwLock::new(Bitfield::empty(pieces)))
        };

        let check_time = std::time::Instant::now();
        let mut torrent_info = Arc::new(torrent_info);

        println!("Checking loaded torrent...");
        utils::check_torrent(&mut torrent_info).await;

        {
            let pieces = torrent_info.torrent_pieces.read().await;
            let total_pieces = pieces.len();
            let missing_pieces = pieces.iter().filter(|p| !p.finished()).count();
    
            println!("Checked torrent in {}s. {}/{} pieces downloaded.", check_time.elapsed().as_secs(), total_pieces - missing_pieces, total_pieces);
        }

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
                    peers.append(&mut t.get_peers().await);
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

                if peer.connect().await {
                    peer.handle_events().await;
                }
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
