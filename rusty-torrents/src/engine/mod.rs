pub mod peer;
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
use tracker::{TrackerKind, TrackerEvent};

use peer::tcp::TcpPeer;
use peer::{Bitfield, Peer, PeerInfo};

pub struct TorrentInfo {
    data: ParsedTorrent,
    piece_length: usize,

    pub total_uploaded: Arc<RwLock<usize>>,
    pub total_downloaded: Arc<RwLock<usize>>,

    torrent_files: Arc<RwLock<Vec<File>>>,
    torrent_pieces: Arc<RwLock<Vec<Piece>>>,
    torrent_peers: Arc<RwLock<Vec<Arc<RwLock<PeerInfo>>>>>,

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

    // Always check that there are unfinished pieces before calling this!
    pub async fn get_unfinished_piece_idx(&self) -> usize {
        if let Some(pieces) = self.get_missing_pieces_count() {
            assert!(pieces > 0)
        }
        else {
            return 0;
        }

        let mut idx: usize;
        let pieces = self.torrent_pieces.read().await;

        let mut rng = rand::thread_rng();
        
        loop {
            idx = rng.gen_range(0..pieces.len());
            
            if !&pieces[idx].requested() && !&pieces[idx].finished() {
                break;
            }
        }
        
        idx
    }

    pub async fn get_pieces_count(&self) -> usize {
        self.torrent_pieces.read().await.len()
    }

    pub fn get_missing_pieces_count(&self) -> Option<usize> {
        if let Ok(pieces) = self.torrent_pieces.try_read() {
            Some(pieces.iter().filter(|p| !p.requested() && !p.finished()).count())
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

    pub fn torrent_peers(&self) -> Arc<RwLock<Vec<Arc<RwLock<PeerInfo>>>>> {
        self.torrent_peers.clone()
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

            total_uploaded: Arc::new(RwLock::new(0)),
            total_downloaded: Arc::new(RwLock::new(0)),

            torrent_files,
            torrent_pieces: Arc::new(RwLock::new(Vec::new())),
            torrent_peers: Arc::new(RwLock::new(Vec::new())),

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
        loop {
            let mut received_peers: Vec<Box<dyn Peer+Send>> = Vec::new();

            for tracker in self.trackers.iter_mut() {
                match tracker {
                    TrackerKind::Tcp(tracker) => {
                        let message = {
                            if tracker.is_announced() {
                                TrackerEvent::PeriodicRequest
                            }
                            else {
                                TrackerEvent::Started
                            }
                        };
    
                        if let Some(result) = tracker.send_message(message).await {
                            for address in result {
                                let mut skip = false;

                                for peer in self.torrent_info.torrent_peers().read().await.iter() {
                                    if peer.read().await.address() == address {
                                        skip = true;
                                        break;
                                    }
                                }

                                if skip {
                                    continue;
                                }

                                let torrent_info = self.torrent_info.clone();
                                let peer_info = Arc::new(RwLock::new(PeerInfo::new(address)));

                                self.torrent_info.torrent_peers.write().await.push(peer_info.clone());
                                received_peers.push(Box::new(TcpPeer::new(address, peer_info, torrent_info).await));
                            }

                            
                        }
                    },
                    TrackerKind::Udp => unimplemented!()
                }
            }

            for peer in received_peers {
                let info = self.torrent_info.clone();
    
                tokio::task::spawn(async move {
                    let info = info;
                    let mut peer = peer;
    
                    if peer.connect().await {
                        loop {
                            if !peer.handle_peer_messages().await {
                                let info = peer.get_peer_info();
    
                                info.write().await.set_active(false);
                                peer.release_requested_piece().await;
    
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
                                    else {
                                        // peer.send_keep_alive().await;
                                    }
                                }
                            }
    
                            if !peer.is_responsive() || peer.is_potato() {
                                let info = peer.get_peer_info();
    
                                info.write().await.set_active(false);
                                peer.release_requested_piece().await;
    
                                break;
                            }
    
                            tokio::task::yield_now().await;
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                });
            }
    
            if let Ok(mut lock) = self.torrent_info.torrent_peers.try_write() {
                let infos = lock.to_owned();
    
                let clear: Vec<Arc<RwLock<PeerInfo>>> = infos.into_iter().filter(|i| {
                    if let Ok(i) = i.try_read() {
                        i.active()
                    }
                    else {
                        true
                    }
                }).collect();
    
                *lock = clear;
            }
    
            std::thread::sleep(std::time::Duration::from_millis(5));
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
