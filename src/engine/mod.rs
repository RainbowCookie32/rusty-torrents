mod peer;
mod tracker;
mod transfer;

use std::path::PathBuf;
use std::net::SocketAddrV4;
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
    // FIXME: This is lazyness. I could create the channel later and have
    // peers_tx be an Option, but lazyness.
    peers_tx: mpsc::Sender<Vec<SocketAddrV4>>,
    peers_rx: mpsc::Receiver<Vec<SocketAddrV4>>,
    
    complete_piece_tx: broadcast::Sender<usize>,
    transfer_progress_tx: broadcast::Sender<TransferProgress>,

    peers_piece_data_tx: mpsc::UnboundedSender<(usize, Vec<u8>)>,
    peers_piece_data_rx: mpsc::UnboundedReceiver<(usize, Vec<u8>)>,

    peers_status: HashMap<SocketAddrV4, PeerStatus>,
    peers_status_rx: HashMap<SocketAddrV4, watch::Receiver<PeerStatus>>,

    peers_cmd_tx: HashMap<SocketAddrV4, mpsc::UnboundedSender<PeerCommand>>,
}

impl Engine {
    pub async fn init(torrent_path: PathBuf, output_path: PathBuf, stop_rx: oneshot::Receiver<()>) -> Engine {
        let torrent_data = fs::read(torrent_path).await.expect("failed to read torrent file");
        let torrent_data =  ParsedTorrent::new(torrent_data);

        let transfer = Transfer::create(torrent_data, output_path.as_path()).await;

        let (peers_tx, peers_rx) = mpsc::channel(5);

        let (complete_piece_tx, _) = broadcast::channel(50);
        let (transfer_progress_tx, _) = broadcast::channel(5);
        let (peers_piece_data_tx, peers_piece_data_rx) = mpsc::unbounded_channel();

        Engine {
            transfer,

            stop_rx,
            peers_tx,
            peers_rx,

            complete_piece_tx,
            transfer_progress_tx,

            peers_piece_data_tx,
            peers_piece_data_rx,

            peers_status: HashMap::new(),
            peers_status_rx: HashMap::new(),

            peers_cmd_tx: HashMap::new()
        }
    }

    pub fn get_progress_rx(&self) -> broadcast::Receiver<TransferProgress> {
        self.transfer_progress_tx.subscribe()
    }

    pub async fn start_torrent(&mut self) {
        let mut complete = false;
        
        self.transfer.check_torrent().await;
        self.start_trackers_task().await;

        loop {
            if self.stop_rx.try_recv().is_ok() {
                // self.send_message_to_trackers(TrackerEvent::Stopped, true).await;
                break;
            }

            if let Ok(list) = self.peers_rx.try_recv() {
                self.add_peers(list).await;
            }

            for (addr, rx) in self.peers_status_rx.iter_mut() {
                if rx.has_changed().unwrap_or_default() {
                    self.peers_status.insert(*addr, rx.borrow_and_update().clone());
                }
            }

            for (addr, status) in self.peers_status.iter_mut() {
                if let PeerStatus::Available { available_pieces } = status {
                    let missing_pieces: Vec<usize> = self.transfer.pieces_status()
                        .iter()
                        .enumerate()
                        .filter(| (_, status) | !(**status))
                        .filter(| (i, _) | !self.transfer.is_piece_assigned(*i))
                        .map(| (i, _) | i)
                        .collect()
                    ;

                    let target_piece = available_pieces
                        .iter()
                        .enumerate()
                        .find(| (i, status) | {
                            if **status {
                                missing_pieces.contains(i)
                            }
                            else {
                                false
                            }
                        })
                    ;

                    if let Some((idx, _)) = target_piece {
                        let mut drop_peer = true;
                        println!("trying to assing piece {idx} to peer {addr}");

                        if let Some(tx) = self.peers_cmd_tx.get(addr) {
                            if !tx.is_closed() && tx.send(PeerCommand::RequestPiece(idx, self.transfer.piece_length())).is_ok() {
                                self.transfer.assign_piece(idx);
                                drop_peer = false;
                            }
                        }

                        if drop_peer {
                            *status = PeerStatus::Dropped { assigned_piece: None };
                        }
                    }
                }
            }

            if let Ok((piece, data)) = self.peers_piece_data_rx.try_recv() {
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
                }
            }

            if self.transfer.is_complete() && !complete {
                complete = true;
                println!("all done, have fun");
                // self.send_message_to_trackers(TrackerEvent::Completed, true).await;
            }
            
            self.clear_peers_list();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    async fn start_trackers_task(&self) {
        let info_hash = self.transfer.info_hash();
        let progress = self.transfer.get_progress();

        let trackers = self.transfer.get_trackers();

        let peers_tx = self.peers_tx.clone();
        let progress_rx = self.transfer_progress_tx.subscribe();

        tokio::spawn(async move {
            let trackers_handler = TrackersHandler::init(info_hash, progress, trackers, peers_tx, progress_rx);
            trackers_handler.start().await;
        });
    }

    async fn add_peers(&mut self, peers: Vec<SocketAddrV4>) {
        let peers: Vec<SocketAddrV4> = peers
            .into_iter()
            .filter(| addr | !self.peers_cmd_tx.contains_key(addr))
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

            self.peers_cmd_tx.insert(address, cmd_tx);
            self.peers_status_rx.insert(address, peer_status_rx);

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

    fn clear_peers_list(&mut self) {
        let peers_to_remove: Vec<(SocketAddrV4, PeerStatus)> = self.peers_status
            .iter()
            .filter(| (_, v) | matches!(*v, PeerStatus::Dropped { assigned_piece: _ }))
            .map(| (k, v) | (*k, v.clone()))
            .collect()
        ;

        if !peers_to_remove.is_empty() {
            println!("dropping {} peers", peers_to_remove.len());

            for (peer, status) in peers_to_remove {
                if let PeerStatus::Dropped { assigned_piece } = status {
                    if let Some(piece) = assigned_piece {
                        self.transfer.unassign_piece(piece);
                    }
                }
    
                self.peers_cmd_tx.remove(&peer);
                self.peers_status.remove(&peer);
                self.peers_status_rx.remove(&peer);
            }
        }
    }
}
