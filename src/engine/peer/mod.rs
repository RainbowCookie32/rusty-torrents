mod connection;
pub mod message;

use std::sync::Arc;
use std::fmt::Display;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tokio::sync::broadcast;

use message::Message;
use connection::PeerConnection;

use crate::engine::utils;
use crate::engine::TorrentInfo;
use crate::engine::tracker::TransferProgress;

pub struct Peer {
    connection: Box<dyn PeerConnection + Send>,

    is_choked: bool,
    is_interested: bool,
    awaiting_response: bool,
    
    peer_choked: bool,
    peer_interested: bool,
    peer_bitfield: Bitfield,

    info_peer: Arc<RwLock<PeerInfo>>,
    info_torrent: Arc<TorrentInfo>,

    requested_size: usize,
    requested_received: usize,
    requested_piece: Option<usize>,

    timer_last_message: Instant,
    timer_received_data: Instant,

    transfer_progress_tx: broadcast::Sender<TransferProgress>
}

impl Peer {
    pub async fn connect(info_peer: Arc<RwLock<PeerInfo>>, info_torrent: Arc<TorrentInfo>, tx: broadcast::Sender<TransferProgress>) -> Option<Peer> {
        let address = info_peer.read().await.address();

        if let Some(mut connection) = connection::create_connection(&address).await {
            let peer_bitfield = Bitfield::empty(info_torrent.pieces_count().await);

            if !connection.handshake_peer(info_torrent.get_handshake()).await {
                return None;
            }

            let bitfield = info_torrent.bitfield_client.read().await.clone();

            if !connection.send_message(Message::Bitfield { bitfield }).await {
                return None;
            }

            let peer = Peer {
                connection,

                is_choked: true,
                is_interested: false,
                awaiting_response: false,

                peer_choked: true,
                peer_interested: false,
                peer_bitfield,

                info_peer,
                info_torrent,

                requested_size: 0,
                requested_received: 0,
                requested_piece: None,

                timer_last_message: Instant::now(),
                timer_received_data: Instant::now(),

                transfer_progress_tx: tx
            };

            Some(peer)
        }
        else {
            None
        }
    }

    pub fn should_drop(&self) -> bool {
        let dead = self.timer_last_message.elapsed() >= Duration::from_secs(10);
        let slow = self.requested_received < (self.requested_size / 2) && self.timer_received_data.elapsed() > Duration::from_secs(30);

        dead || slow
    }

    pub fn should_request_piece(&self) -> bool {
        !self.awaiting_response && !self.is_choked
    }

    pub fn requested_piece(&self) -> Option<usize> {
        self.requested_piece
    }

    pub async fn release_requested_piece(&mut self) {
        if let Some(piece) = self.requested_piece {
            self.info_torrent.release_piece(piece).await;
            
            self.requested_size = 0;
            self.requested_received = 0;
            self.requested_piece = None;
        }
    }

    pub async fn request_piece(&mut self, piece: usize) -> bool {
        if self.peer_bitfield.is_piece_available(piece) {
            if self.awaiting_response {
                return true;
            }

            if self.peer_choked && !self.connection.send_message(Message::Unchoke).await {
                self.info_peer.write().await.set_last_message_sent(Message::Unchoke);
                return false;
            }

            if !self.is_interested && !self.connection.send_message(Message::Interested).await {
                self.info_peer.write().await.set_last_message_sent(Message::Interested);
                return false;
            }

            let (block_offset, block_length) = {
                if let Some(piece) = self.info_torrent.pieces.read().await.get(piece) {
                    piece.get_block_request()
                }
                else {
                    return false;
                }
            };

            let message = Message::Request {
                piece_idx: piece as u32,
                block_offset,
                block_length
            };

            if self.connection.send_message(message.clone()).await {
                self.requested_piece = Some(piece);
                
                self.requested_size = 0;
                self.requested_received = 0;
                self.awaiting_response = true;
                self.timer_received_data = Instant::now();

                self.info_peer.write().await.set_last_message_sent(message);

                true
            }
            else {
                false
            }
        }
        else {
            false
        }
    }

    pub async fn handle_messages(&mut self) -> bool {
        if let Some(message) = self.connection.get_message().await {
            self.timer_last_message = Instant::now();
            self.info_peer.write().await.set_last_message_received(message.clone());

            match message {
                Message::KeepAlive => {}

                Message::Choke => self.is_choked = true,
                Message::Unchoke => self.is_choked = false,
                Message::Interested => self.peer_interested = true,
                Message::NotInterested => self.peer_interested = false,

                Message::Have { piece } => {
                    // Piece doesn't exist on the torrent, drop the peer.
                    if piece > self.info_torrent.pieces.read().await.len() as u32 {
                        return false;
                    }
                    
                    self.peer_bitfield.piece_finished(piece);
                }
                Message::Bitfield { bitfield } => {
                    self.peer_bitfield = bitfield;
                }
                Message::Request { piece_idx, block_offset, block_length } => {
                    if self.info_torrent.bitfield_client.read().await.is_piece_available(piece_idx as usize) {
                        if !self.is_choked && self.peer_interested {
                            if let Some(piece) = self.info_torrent.pieces.read().await.get(piece_idx as usize) {
                                let (start_file, start_position) = piece.get_offsets();
                                let piece = utils::read_piece(self.info_torrent.clone(), start_file, start_position).await;
                                let block_data = {
                                    let block_offset = block_offset as usize;
                                    let block_length = block_length as usize;

                                    piece[block_offset..block_offset+block_length].to_vec()
                                };

                                let message = Message::Piece {
                                    piece_idx,
                                    block_offset,
                                    block_data
                                };

                                if self.connection.send_message(message).await {
                                    self.info_peer.write().await.add_uploaded(block_length as usize);
                                    *self.info_torrent.total_uploaded.write().await += block_length as usize;
                                }
                                else {
                                    return false;
                                }
                            }
                        }
                    }
                    else {
                        // Requested a piece we don't have, dropping.
                        return false;
                    }
                }
                Message::Piece { piece_idx,block_data, .. } => {
                    if let Some(requested) = self.requested_piece.as_ref() {
                        if *requested != piece_idx as usize {
                            // Mismatched piece, drop the peer.
                            return false;
                        }
                    }
                    else {
                        // Unsolicited data, drop the peer just in case.
                        return false;
                    }

                    let (mut file_idx, mut file_position) = (0, 0);
                    let mut piece_data = None;

                    {
                        let mut lock = self.info_torrent.pieces.write().await;

                        if let Some(piece) = lock.get_mut(piece_idx as usize) {
                            self.requested_received += block_data.len();
                            self.info_peer.write().await.add_downloaded(block_data.len());
                            *self.info_torrent.total_downloaded.write().await += block_data.len();
    
                            if piece.add_received_bytes(block_data) {
                                if piece.check_piece() {
                                    file_idx = piece.get_offsets().0;
                                    file_position = piece.get_offsets().1;
                                    piece_data = Some(piece.data().to_owned());
                                    
                                    piece.set_finished(true);
                                    piece.set_requested(false);
                                }
                                else {
                                    piece.discard();
                                }
    
                                self.requested_size = 0;
                                self.requested_piece = None;
                            }
                        }
                        else {
                            return false;
                        }
                    }

                    self.awaiting_response = false;

                    if let Some(piece_data) = piece_data {
                        let total: usize = self.info_torrent.pieces().read().await
                            .iter()
                            .map(| piece | piece.length())
                            .sum()
                        ;

                        let downloaded: usize = self.info_torrent.pieces().read().await
                            .iter()
                            .filter(| piece | piece.finished())
                            .map(| piece | piece.length())
                            .sum()
                        ;

                        let progress = TransferProgress::new((total - downloaded) as u64, 0, 0);

                        self.transfer_progress_tx.send(progress).expect("failed to send transfer progress");
                        utils::write_piece(self.info_torrent.clone(), &piece_data, &mut file_idx, &mut file_position).await;
                    }
                }
                Message::Cancel { .. } => todo!(),
            }
        }

        true
    }
}

#[derive(Clone, PartialEq)]
pub enum ConnectionStatus {
    Dropped,
    Connected,
    Connecting,
    Disconnected
}

impl Display for ConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionStatus::Dropped => write!(f, "Dropped"),
            ConnectionStatus::Connected => write!(f, "Connected"),
            ConnectionStatus::Connecting => write!(f, "Connecting"),
            ConnectionStatus::Disconnected => write!(f, "Disconnected"),
        }
    }
}

#[derive(Clone)]
pub struct PeerInfo {
    status: ConnectionStatus,

    uploaded_total: usize,
    downloaded_total: usize,

    last_message_sent: Option<Message>,
    last_message_received: Option<Message>,

    address: SocketAddrV4,
    start_time: Instant,
}

impl PeerInfo {
    pub fn new(address: SocketAddrV4) -> PeerInfo {
        PeerInfo {
            status: ConnectionStatus::Disconnected,

            uploaded_total: 0,
            downloaded_total: 0,

            last_message_sent: None,
            last_message_received: None,

            address,
            start_time: Instant::now()
        }
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status.clone()
    }

    pub fn uploaded_total(&self) -> usize {
        self.uploaded_total
    }

    pub fn downloaded_total(&self) -> usize {
        self.downloaded_total
    }

    pub fn last_message_sent(&self) -> Option<&Message> {
        self.last_message_sent.as_ref()
    }

    pub fn last_message_received(&self) -> Option<&Message> {
        self.last_message_received.as_ref()
    }

    pub fn address(&self) -> SocketAddrV4 {
        self.address
    }

    pub fn set_status(&mut self, status: ConnectionStatus) {
        self.status = status;
    }

    pub fn set_last_message_sent(&mut self, last_message: Message) {
        self.last_message_sent = Some(last_message);
    }

    pub fn set_last_message_received(&mut self, last_message_received: Message) {
        self.last_message_received = Some(last_message_received);
    }

    pub fn add_uploaded(&mut self, value: usize) {
        self.uploaded_total += value;
    }

    pub fn add_downloaded(&mut self, value: usize) {
        self.downloaded_total += value;
    }

    pub fn start_time(&self) -> &Instant {
        &self.start_time
    }
}

#[derive(Clone)]
pub struct Bitfield {
    pieces: Vec<bool>
}

impl Bitfield {
    pub fn empty(pieces: usize) -> Bitfield {
        Bitfield {
            pieces: vec![false; pieces]
        }
    }

    pub fn from_peer_data(data: Vec<u8>) -> Bitfield {
        let mut result = Vec::new();

        for byte in data {
            for bit in (0..8).rev() {
                result.push((byte >> bit) & 1 == 1);
            }
        }

        Bitfield {
            pieces: result
        }
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        let mut idx = 0;
        let amount = self.pieces.len() as f32 / 8.0;
        let mut result = vec![0; amount.ceil() as usize];
        
        for byte in result.iter_mut() {
            for bit in (0..8).rev() {
                if idx >= self.pieces.len() {
                    break;
                }
                else {
                    if self.pieces[idx] {
                        *byte |= 1 << bit;
                    }
    
                    idx += 1;
                }
            }
        }

        result
    }

    pub fn is_piece_available(&self, piece: usize) -> bool {
        self.pieces[piece]
    }

    pub fn piece_finished(&mut self, piece: u32) {
        self.pieces[piece as usize] = true;
    }
}
