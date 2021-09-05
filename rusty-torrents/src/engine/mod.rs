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
    pub async fn release_piece(&self, idx: usize) {
        if let Some(piece) = self.torrent_pieces.write().await.get_mut(idx) {
            piece.set_requested(false);
        }
    }

    pub async fn piece_requested(&self, idx: usize) {
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

    pub fn get_missing_pieces_count(&self) -> Option<usize> {
        if let Ok(pieces) = self.torrent_pieces.try_read() {
            Some(pieces.iter().filter(|p| !p.finished()).count())
        }
        else {
            None
        }
        
    }

    /// Get a reference to the torrent info's data.
    pub fn data(&self) -> &ParsedTorrent {
        &self.data
    }

    pub fn torrent_pieces(&self) -> Arc<RwLock<Vec<Piece>>> {
        self.torrent_pieces.clone()
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

        let pieces = data.info().pieces().len();

        let torrent_info = TorrentInfo {
            data,
            piece_length,

            torrent_files,
            torrent_pieces: Arc::new(RwLock::new(Vec::new())),

            bitfield_client: Arc::new(RwLock::new(Bitfield::empty(pieces)))
        };

        let mut torrent_info = Arc::new(torrent_info);

        utils::check_torrent(&mut torrent_info).await;

        let trackers = tracker::create_trackers(trackers_list, torrent_info.clone());

        Engine {
            trackers,
            torrent_info
        }
    }

    pub fn info(&self) -> Arc<TorrentInfo> {
        self.torrent_info.clone()
    }

    pub async fn start_torrent(&mut self) {
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

        for peer in peers {
            let info = self.torrent_info.clone();

            tokio::task::spawn(async move {
                let info = info;
                let mut peer = peer;

                if peer.connect().await {
                    loop {
                        if !peer.handle_peer_messages().await {
                            let piece = peer.get_assigned_piece();
                            
                            if let Some(piece) = piece {
                                info.release_piece(piece).await;
                            }

                            break;
                        }

                        tokio::task::yield_now().await;

                        if peer.should_request() {
                            let piece = peer.get_assigned_piece();

                            if let Some(piece) = piece {
                                if peer.request_piece(piece).await {
                                    info.piece_requested(piece).await;
                                }
                            }
                            else if let Some(missing_pieces) = info.get_missing_pieces_count() {
                                if missing_pieces > 0 {
                                    let piece = info.get_unfinished_piece_idx().await;

                                    if peer.request_piece(piece).await {
                                        info.piece_requested(piece).await;
                                    }
                                }
                            }
                        }

                        tokio::task::yield_now().await;
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            });
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
