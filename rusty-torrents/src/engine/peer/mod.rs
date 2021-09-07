pub mod tcp;

use std::sync::Arc;
use std::fmt::Display;
use std::time::Instant;
use std::net::SocketAddrV4;

use bytes::Buf;
use async_trait::async_trait;
use tokio::sync::RwLock;

#[async_trait]
pub trait Peer {
    async fn connect(&mut self) -> bool;
    async fn send_keep_alive(&mut self) -> bool;
    async fn release_requested_piece(&mut self);
    async fn handle_peer_messages(&mut self) -> bool;
    async fn request_piece(&mut self, piece: usize) -> bool;

    fn is_potato(&self) -> bool;
    fn is_responsive(&self) -> bool;
    fn should_request(&self) -> bool;
    fn get_assigned_piece(&self) -> Option<usize>;
    fn get_peer_info(&self) -> Arc<RwLock<PeerInfo>>;
}

pub struct PeerInfo {
    active: bool,

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
            active: true,

            uploaded_total: 0,
            downloaded_total: 0,

            last_message_sent: None,
            last_message_received: None,

            address,
            start_time: Instant::now()
        }
    }

    pub fn active(&self) -> bool {
        self.active
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

    pub fn set_active(&mut self, active: bool) {
        self.active = active;
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

pub struct PeerStatus {
    peer_choked: bool,
    peer_interested: bool,

    client_choked: bool,
    client_interested: bool
}

impl PeerStatus {
    pub fn new() -> PeerStatus {
        PeerStatus {
            peer_choked: true,
            peer_interested: false,

            client_choked: true,
            client_interested: false
        }
    }
}

pub struct Bitfield {
    pieces: Vec<bool>
}

impl Bitfield {
    pub fn empty(pieces: usize) -> Bitfield {
        Bitfield {
            pieces: vec![false; pieces]
        }
    }

    pub fn from_peer_data(pieces: Vec<u8>) -> Bitfield {
        let mut result = Vec::new();

        for byte in pieces {
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

    pub fn is_piece_available(&self, piece: u32) -> bool {
        self.pieces[piece as usize]
    }

    pub fn piece_finished(&mut self, piece: u32) {
        self.pieces[piece as usize] = true;
    }
}

#[derive(Clone)]
pub enum Message {
    KeepAlive,

    Choke,
    Unchoke,
    Interested,
    NotInterested,

    Have(Vec<u8>),
    Bitfield(Vec<u8>),
    Request(Vec<u8>),
    Piece(Vec<u8>),
    Cancel(Vec<u8>)
}

impl Message {
    pub fn from_data(data: Vec<u8>) -> Option<Message> {
        let mut data = data;
        let length = data.as_slice().get_u32();

        // ! This is kinda horrible.
        for _ in 0..4 {
            data.remove(0);
        }

        if length == 0 {
            Some(Message::KeepAlive)
        }
        else {
            match data.remove(0) {
                0 => Some(Message::Choke),
                1 => Some(Message::Unchoke),
                2 => Some(Message::Interested),
                3 => Some(Message::NotInterested),

                4 => Some(Message::Have(data)),
                5 => Some(Message::Bitfield(data)),
                6 => Some(Message::Request(data)),
                7 => Some(Message::Piece(data)),
                8 => Some(Message::Cancel(data)),
                
                _ => None
            }
        }
    }
}

impl Display for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Message::KeepAlive => write!(f, "Keep Alive"),
            Message::Choke => write!(f, "Choke"),
            Message::Unchoke => write!(f, "Unchoke"),
            Message::Interested => write!(f, "Interested"),
            Message::NotInterested => write!(f, "Not Interested"),
            Message::Have(_) => write!(f, "Have"),
            Message::Bitfield(_) => write!(f, "Bitfield"),
            Message::Request(_) => write!(f, "Request"),
            Message::Piece(_) => write!(f, "Piece"),
            Message::Cancel(_) => write!(f, "Cancel")
        }
    }
}
