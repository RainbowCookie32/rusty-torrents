pub mod tcp;

use bytes::Buf;
use async_trait::async_trait;

#[async_trait]
pub trait Peer {
    async fn connect(&mut self) -> bool;
    async fn handle_events(&mut self);
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
