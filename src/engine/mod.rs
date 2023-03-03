mod peer;
mod tracker;
mod transfer;

use std::path::PathBuf;
use std::net::SocketAddr;
use std::collections::HashMap;

use tokio::fs;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use tracker::TrackersHandler;
use peer::{PeerCommand, PeerStatus};
use transfer::{Transfer, TransferProgress};

use crate::bencode::ParsedTorrent;

pub struct Engine {
    transfer: Transfer,

    stop_rx: oneshot::Receiver<()>,
    peers_controls: HashMap<SocketAddr, PeerControl>,
    
    complete_piece_tx: broadcast::Sender<usize>,
    transfer_progress_tx: broadcast::Sender<TransferProgress>,

    peers_piece_data_tx: mpsc::UnboundedSender<(usize, Vec<u8>)>,
    peers_piece_data_rx: mpsc::UnboundedReceiver<(usize, Vec<u8>)>,
}

impl Engine {
    pub async fn init(torrent_path: PathBuf, output_path: PathBuf, stop_rx: oneshot::Receiver<()>) -> Engine {
        let torrent_data = fs::read(torrent_path).await.expect("failed to read torrent file");
        let torrent_data =  ParsedTorrent::new(torrent_data);

        let transfer = Transfer::create(torrent_data, output_path.as_path()).await;

        let (complete_piece_tx, _) = broadcast::channel(50);
        let (transfer_progress_tx, _) = broadcast::channel(5);
        let (peers_piece_data_tx, peers_piece_data_rx) = mpsc::unbounded_channel();

        Engine {
            transfer,

            stop_rx,
            peers_controls: HashMap::new(),

            complete_piece_tx,
            transfer_progress_tx,

            peers_piece_data_tx,
            peers_piece_data_rx,
        }
    }

    pub fn get_progress_rx(&self) -> broadcast::Receiver<TransferProgress> {
        self.transfer_progress_tx.subscribe()
    }

    pub async fn start_torrent(&mut self) {
        let mut complete = false;
        self.transfer.check_torrent().await;
        
        let mut peers_rx = self.start_trackers_task().await;

        loop {
            if self.stop_rx.try_recv().is_ok() {
                for (_, control) in self.peers_controls.iter_mut() {
                    control.send_cmd(PeerCommand::Disconnect);
                }

                // TODO: Send stopped event to Trackers.
                break;
            }

            if let Ok(list) = peers_rx.try_recv() {
                self.add_peers(list).await;
            }

            for (addr, control) in self.peers_controls.iter_mut() {
                control.update_status();
                
                match &control.status {
                    PeerStatus::Connected { available_pieces } => {
                        let is_relevant = self.transfer.get_piece_for_peer(available_pieces).is_some();

                        if is_relevant {
                            control.send_cmd(PeerCommand::SendInterested);
                        }
                    }
                    PeerStatus::Available { available_pieces } => {
                        let mut pieces_idx = Vec::with_capacity(5);
                        let mut pieces = Vec::with_capacity(5);

                        while let Some(piece) = self.transfer.get_piece_for_peer(available_pieces) {
                            println!("assigned piece {} to peer {addr}", piece.idx());
                                    
                            piece.set_assigned(*addr);
                            pieces_idx.push(piece.idx());
                            pieces.push((piece.idx(), piece.length() as u64));

                            if pieces.len() == 5 {
                                break;
                            }
                        }

                        if !pieces.is_empty() {
                            control.assign_pieces(pieces);
                        }
                    }
                    PeerStatus::RequestedPiece { piece } => {
                        let data = self.transfer.read_piece(*piece).await;
                        control.send_cmd(PeerCommand::SendPiece { piece: *piece, data });
                    }
                    _ => {}
                }
            }

            if let Ok((piece, data)) = self.peers_piece_data_rx.try_recv() {
                let peer = self.peers_controls.values_mut()
                    .find(| v | v.has_piece_assigned(piece))
                ;

                if let Some(peer) = peer {
                    let idx = peer.assigned_pieces.iter()
                        .enumerate()
                        .find(| (_, (piece_idx, _)) | piece == *piece_idx)
                        .map(| (idx, _) | idx)
                        .unwrap()
                    ;
                    
                    peer.assigned_pieces.remove(idx);
                }

                if self.transfer.add_complete_piece(piece, data).await {
                    println!("piece {piece} was written to disk");

                    self.complete_piece_tx
                        .send(piece)
                        .expect("failed to send complete piece message")
                    ;

                    self.transfer_progress_tx
                        .send(self.transfer.get_progress())
                        .expect("failed to send TransferProgress message")
                    ;
                }
                else {
                    println!("piece was invalid");
                    self.transfer.unassign_piece(piece);
                }
            }

            if self.transfer.is_complete() && !complete {
                complete = true;
                println!("all done, have fun");
                // self.send_message_to_trackers(TrackerEvent::Completed, true).await;
            }
            
            self.purge_peers_list();

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn start_trackers_task(&self) -> mpsc::Receiver<Vec<SocketAddr>> {
        let info_hash = self.transfer.info_hash();
        let progress = self.transfer.get_progress();

        let trackers = self.transfer.get_trackers();

        let (peers_tx, peers_rx) = mpsc::channel(5);
        let progress_rx = self.transfer_progress_tx.subscribe();

        tokio::spawn(async move {
            let trackers_handler = TrackersHandler::init(info_hash, progress, trackers, peers_tx, progress_rx);
            trackers_handler.start().await;
        });

        peers_rx
    }

    async fn add_peers(&mut self, peers: Vec<SocketAddr>) {
        let peers: Vec<SocketAddr> = peers
            .into_iter()
            .filter(| addr | !self.peers_controls.contains_key(addr))
            .collect()
        ;

        println!("deduplicated peers, new list is {}", peers.len());

        for address in peers {
            let info_hash = self.transfer.info_hash();
            let completed_pieces = self.transfer.pieces_status().clone();

            let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let (peer_status_tx, peer_status_rx) = watch::channel(PeerStatus::Waiting);
            
            let complete_piece_rx = self.complete_piece_tx.subscribe();
            let complete_piece_data_tx = self.peers_piece_data_tx.clone();

            let peer_control = PeerControl {
                status: PeerStatus::Waiting,
                assigned_pieces: Vec::with_capacity(5),

                cmd_tx,
                status_rx: peer_status_rx
            };

            self.peers_controls.insert(address, peer_control);

            tokio::spawn(async move {
                let new_peer = peer::TcpPeer::connect(
                    address,
                    info_hash,
                    completed_pieces,
                    cmd_rx,
                    peer_status_tx,
                    complete_piece_rx,
                    complete_piece_data_tx
                ).await;
    
                if let Some(peer) = new_peer {
                    peer.connect_to_peer().await;
                }
            });
        }
    }

    /// Goes through each PeerControl and checks if the PeerStatus is Dropped.
    /// Release the pieces in case it is.
    fn purge_peers_list(&mut self) {
        let peers_to_remove: Vec<SocketAddr> = self.peers_controls
            .iter()
            .filter(| (_, v) | v.cmd_tx.is_closed() || v.status == PeerStatus::Dropped)
            .map(| (k, _) | (*k))
            .collect()
        ;

        if !peers_to_remove.is_empty() {
            println!("dropping {} peers", peers_to_remove.len());

            for peer_address in peers_to_remove {
                if let Some(control) = self.peers_controls.remove(&peer_address) {
                    for (piece, _) in control.assigned_pieces {
                        self.transfer.unassign_piece(piece);
                    }
                }
            }
        }
    }
}

pub struct PeerControl {
    status: PeerStatus,
    assigned_pieces: Vec<(usize, u64)>,
    
    cmd_tx: mpsc::UnboundedSender<PeerCommand>,
    status_rx: watch::Receiver<PeerStatus>,
}

impl PeerControl {
    fn send_cmd(&mut self, cmd: PeerCommand) {
        if self.cmd_tx.send(cmd).is_err() {
            println!("failed to send cmd to peer, dropping...");
            self.status = PeerStatus::Dropped;
        }
    }

    fn has_piece_assigned(&self, piece: usize) -> bool {
        self.assigned_pieces.iter()
            .any(| (idx, _) | *idx == piece)
    }

    fn assign_pieces(&mut self, pieces: Vec<(usize, u64)>) {
        self.assigned_pieces = pieces.clone();
        self.send_cmd(PeerCommand::RequestPiece { pieces });
    }

    fn update_status(&mut self) {
        match self.status_rx.has_changed() {
            Ok(has_changed) => {
                if has_changed {
                    self.status = self.status_rx.borrow_and_update().clone();
                }
            }
            Err(_) => {
                self.status = PeerStatus::Dropped;
            }
        }
    }
}
