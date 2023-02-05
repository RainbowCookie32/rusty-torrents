use std::fmt::Display;

use bytes::Buf;

use crate::engine::peer::Bitfield;

#[derive(Clone)]
pub enum Message {
    KeepAlive,

    Choke,
    Unchoke,
    Interested,
    NotInterested,

    Have { piece: u32 },
    Bitfield { bitfield: Bitfield },
    Request { piece_idx: u32, block_offset: u32, block_length: u32 },
    Piece { piece_idx: u32, block_offset: u32, block_data: Vec<u8> },
    Cancel { piece_idx: u32, block_offset: u32, block_length: u32 }
}

impl Message {
    pub fn from_bytes(data: Vec<u8>) -> Option<Message> {
        let mut data = data;

        if data.is_empty() {
            Some(Message::KeepAlive)
        }
        else {
            let id = data.remove(0);
            let mut data = data.as_slice();

            match id {
                0 => Some(Message::Choke),
                1 => Some(Message::Unchoke),
                2 => Some(Message::Interested),
                3 => Some(Message::NotInterested),

                4 => {
                    let piece = data.get_u32();
                    
                    Some(Message::Have { piece })
                }
                5 => {
                    let bitfield = Bitfield::from_peer_data(data.to_vec());

                    Some(Message::Bitfield { bitfield })
                }
                6 => {
                    let piece_idx = data.get_u32();
                    let block_offset = data.get_u32();
                    let block_length = data.get_u32();

                    Some(Message::Request { piece_idx, block_offset, block_length })
                }
                7 => {
                    let piece_idx = data.get_u32();
                    let block_offset = data.get_u32();
                    let block_data = data.to_vec();

                    Some(Message::Piece { piece_idx, block_offset, block_data })
                }
                8 => {
                    let piece_idx = data.get_u32();
                    let block_offset = data.get_u32();
                    let block_length = data.get_u32();

                    Some(Message::Cancel { piece_idx, block_offset, block_length })
                }
                
                _ => None
            }
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Message::KeepAlive => vec![0, 0, 0, 0, 0],
            Message::Choke => vec![0, 0, 0, 1, 0],
            Message::Unchoke => vec![0, 0, 0, 1, 1],
            Message::Interested => vec![0, 0, 0, 1, 2],
            Message::NotInterested => vec![0, 0, 0, 1, 3],
            
            Message::Have { piece } => {
                let mut message = Vec::new();
                
                // Length (1 for Message ID + 4 for the Piece index).
                message.append(&mut 5_u32.to_be_bytes().to_vec());
                message.push(4);
                // Piece index.
                message.append(&mut piece.to_be_bytes().to_vec());

                message
            }
            Message::Bitfield { bitfield } => {
                let mut bitfield = bitfield.as_bytes();
                let mut message = Vec::new();

                let len = bitfield.len() as u32 + 1;

                message.append(&mut len.to_be_bytes().to_vec());
                message.push(5);
                message.append(&mut bitfield);

                message
            }
            Message::Request { piece_idx, block_offset, block_length } => {
                let mut message = Vec::new();
                
                // Length (1 for Message ID + 4 for the Piece index + 8 for block offset and length).
                message.append(&mut 13_u32.to_be_bytes().to_vec());
                message.push(6);
                message.append(&mut piece_idx.to_be_bytes().to_vec());
                message.append(&mut block_offset.to_be_bytes().to_vec());
                message.append(&mut block_length.to_be_bytes().to_vec());

                message
            }
            Message::Piece { piece_idx, block_offset, block_data } => {
                let mut message = Vec::new();
                // 1 for Message ID, 4 for piece idx, 4 for block offset, then block length.
                let length = 9 + block_data.len() as u32;
                
                message.append(&mut length.to_be_bytes().to_vec());
                message.push(7);
                message.append(&mut piece_idx.to_be_bytes().to_vec());
                message.append(&mut block_offset.to_be_bytes().to_vec());
                message.append(&mut block_data.to_owned());

                message
            }
            Message::Cancel { piece_idx, block_offset, block_length } => {
                let mut message = Vec::new();
                
                // Length (1 for Message ID + 4 for the Piece index + 8 for block offset and length).
                message.append(&mut 13_u32.to_be_bytes().to_vec());
                message.push(8);
                message.append(&mut piece_idx.to_be_bytes().to_vec());
                message.append(&mut block_offset.to_be_bytes().to_vec());
                message.append(&mut block_length.to_be_bytes().to_vec());

                message
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
            Message::Have {..} => write!(f, "Have"),
            Message::Bitfield {..} => write!(f, "Bitfield"),
            Message::Request {..} => write!(f, "Request"),
            Message::Piece {..} => write!(f, "Piece"),
            Message::Cancel {..} => write!(f, "Cancel")
        }
    }
}