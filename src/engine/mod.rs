pub mod peer;
mod file;
mod piece;
mod utils;
mod tracker;

use std::sync::Arc;
use std::net::SocketAddrV4;

use rand::Rng;

use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

use file::File;
use piece::Piece;
use peer::{Bitfield, ConnectionStatus, Peer, PeerInfo};

use crate::bencode::ParsedTorrent;

use self::tracker::TrackersHandler;
pub use self::tracker::TransferProgress;

pub struct TorrentInfo {
    data: ParsedTorrent,
    piece_length: u64,

    pub total_uploaded: Arc<RwLock<usize>>,
    pub total_downloaded: Arc<RwLock<usize>>,

    torrent_files: Arc<RwLock<Vec<File>>>,
    torrent_pieces: Arc<RwLock<Vec<Piece>>>,
    torrent_peers: Arc<RwLock<Vec<Arc<RwLock<PeerInfo>>>>>,

    bitfield_client: Arc<RwLock<Bitfield>>
}

impl TorrentInfo {
    pub fn get_handshake(&self) -> Vec<u8> {
        let id: String = vec!['e'; 20].iter().collect();
        let info_hash = self.data.info().info_hash();
        let mut handshake = b"BitTorrent protocol".to_vec();

        handshake.insert(0, 19);

        for offset in 0..8 {
            handshake.insert(20 + offset, 0);
        }
    
        for (offset, byte) in info_hash.iter().enumerate() {
            handshake.insert(28 + offset, *byte);
        }
        
        for (offset, c) in id.chars().enumerate() {
            handshake.insert(48 + offset, c as u8);
        }

        handshake
    }

    pub fn is_complete(&self) -> bool {
        if let Ok(pieces) = self.torrent_pieces.try_read() {
            pieces.iter().filter(|p| !p.finished()).count() == 0
        }
        else {
            false
        }
    }

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
    stop_rx: oneshot::Receiver<()>,
    peers_rx: mpsc::Receiver<Vec<SocketAddrV4>>,
    transfer_progress_tx: broadcast::Sender<TransferProgress>,

    torrent_info: Arc<TorrentInfo>,
}

impl Engine {
    pub async fn init(data: Vec<u8>, stop_rx: oneshot::Receiver<()>) -> Engine {
        let data = ParsedTorrent::new(data);
        let piece_length = data.info().piece_length();
        
        let mut trackers_list: Vec<String> = {
            data.announce_list()
                .iter()
                .filter(| url | !url.is_empty())
                .map(| url | url.to_owned())
                .collect()
        };

        trackers_list.insert(0, data.announce().to_owned());
        
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

        let torrent_info = Arc::new(torrent_info);
        
        let (peers_tx, peers_rx) = mpsc::channel(5);
        let (transfer_progress_tx, transfer_progress_rx) = broadcast::channel(5);

        utils::check_torrent(torrent_info.clone()).await;

        let total: usize = torrent_info.torrent_pieces().read().await
            .iter()
            .map(| piece | piece.get_len())
            .sum()
        ;

        let downloaded: usize = torrent_info.torrent_pieces().read().await
            .iter()
            .filter(| piece | piece.finished())
            .map(| piece | piece.get_len())
            .sum()
        ;
        
        let progress = TransferProgress::new((total - downloaded) as u64, 0, 0);

        transfer_progress_tx.send(progress.clone()).unwrap();

        let trackers_handler = TrackersHandler::init(
            *torrent_info.data().info().info_hash(),
            progress,
            trackers_list,
            peers_tx,
            transfer_progress_rx
        );

        tokio::spawn(async move {
            trackers_handler.start().await;
        });

        Engine {
            stop_rx,
            peers_rx,
            transfer_progress_tx,

            torrent_info
        }
    }

    pub fn info(&self) -> Arc<TorrentInfo> {
        self.torrent_info.clone()
    }

    pub fn get_progress_rx(&self) -> broadcast::Receiver<TransferProgress> {
        self.transfer_progress_tx.subscribe()
    }

    pub async fn start_torrent(&mut self) {
        let mut complete = false;

        loop {
            if self.stop_rx.try_recv().is_ok() {
                // self.send_message_to_trackers(TrackerEvent::Stopped, true).await;
                break;
            }

            if let Ok(list) = self.peers_rx.try_recv() {
                self.add_peers(list).await;
            }

            for info in self.torrent_info.torrent_peers.read().await.iter() {
                if info.read().await.status() != ConnectionStatus::Disconnected {
                    continue;
                }

                let info_peer = info.clone();
                let info_torrent = self.torrent_info.clone();
                let tx = self.transfer_progress_tx.clone();
    
                tokio::spawn(async move {
                    let info_peer = info_peer;
                    let info_torrent = info_torrent;

                    info_peer.write().await.set_status(ConnectionStatus::Connecting);

                    if let Some(mut peer) = Peer::connect(info_peer.clone(), info_torrent.clone(), tx).await {
                        info_peer.write().await.set_status(ConnectionStatus::Connected);

                        loop {
                            if !peer.handle_messages().await {
                                peer.release_requested_piece().await;
                                info_peer.write().await.set_status(ConnectionStatus::Dropped);
    
                                break;
                            }
        
                            if peer.should_request_piece() {
                                let piece = peer.get_requested_piece();
    
                                if let Some(piece) = piece {
                                    if peer.request_piece(piece).await {
                                        info_torrent.piece_requested(piece).await;
                                    }
                                }
                                else if let Some(missing_pieces) = info_torrent.get_missing_pieces_count() {
                                    if missing_pieces > 0 {
                                        let piece = info_torrent.get_unfinished_piece_idx().await;
    
                                        if peer.request_piece(piece).await {
                                            info_torrent.piece_requested(piece).await;
                                        }
                                    }
                                    else {
                                        // peer.send_keep_alive().await;
                                    }
                                }
                            }
    
                            if peer.should_drop() {
                                peer.release_requested_piece().await;
                                info_peer.write().await.set_status(ConnectionStatus::Dropped);
    
                                break;
                            }
    
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                    }
                    else {
                        info_peer.write().await.set_status(ConnectionStatus::Dropped);
                    }
                });
            }

            if self.torrent_info.is_complete() && !complete {
                complete = true;
                // self.send_message_to_trackers(TrackerEvent::Completed, true).await;
            }
            
            self.clear_peers_list().await;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    async fn add_peers(&mut self, peers: Vec<SocketAddrV4>) {
        for address in peers {
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

            let info_peer = Arc::new(RwLock::new(PeerInfo::new(address)));
            self.torrent_info.torrent_peers.write().await.push(info_peer);
        }
    }

    async fn clear_peers_list(&mut self) {
        if let Ok(mut lock) = self.torrent_info.torrent_peers.try_write() {
            let infos = lock.to_owned();

            *lock = infos
                .into_iter()
                .filter(| i | {
                    if let Ok(i) = i.try_read() {
                        i.status() != ConnectionStatus::Dropped
                    }
                    else {
                        true
                    }
                })
                .collect()
            ;
        }
    }

    async fn build_file_list(files_data: &[(std::string::String, u64)], piece_length: u64) -> Vec<File> {
        let mut list = Vec::new();

        for (filename, size) in files_data {
            list.push(File::new(filename.to_string(), *size, piece_length).await);
        }

        list
    }
}
