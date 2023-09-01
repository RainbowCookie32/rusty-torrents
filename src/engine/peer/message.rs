use std::fmt::Display;

use bytes::*;

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

impl From<Bytes> for Message {
    fn from(data: Bytes) -> Self {
        let mut data = data;

        if data.is_empty() {
            return Message::KeepAlive;
        }

        let msg_kind = data.get_u8();

        match msg_kind {
            0 => Message::Choke,
            1 => Message::Unchoke,
            2 => Message::Interested,
            3 => Message::NotInterested,
            4 => Message::Have { piece: data.get_u32() },
            5 => Message::Bitfield { bitfield: Bitfield::from_peer_data(&data) },
            6 => {
                let piece_idx = data.get_u32();
                let block_offset = data.get_u32();
                let block_length = data.get_u32();

                Message::Request { piece_idx, block_offset, block_length }
            }
            7 => {
                let piece_idx = data.get_u32();
                let block_offset = data.get_u32();
                let block_data = data.to_vec();

                Message::Piece { piece_idx, block_offset, block_data }
            }
            8 => {
                let piece_idx = data.get_u32();
                let block_offset = data.get_u32();
                let block_length = data.get_u32();

                Message::Cancel { piece_idx, block_offset, block_length }
            }
            _ => unreachable!()
        }
    }
}

impl From<Message> for Bytes {
    fn from(msg: Message) -> Bytes {
        match msg {
            Message::KeepAlive => Bytes::from(vec![0, 0, 0, 0, 0]),
            Message::Choke => Bytes::from(vec![0, 0, 0, 1, 0]),
            Message::Unchoke => Bytes::from(vec![0, 0, 0, 1, 1]),
            Message::Interested => Bytes::from(vec![0, 0, 0, 1, 2]),
            Message::NotInterested => Bytes::from(vec![0, 0, 0, 1, 3]),
            
            Message::Have { piece } => {
                // Length (4 bytes) + message kind (1 byte) + piece idx (4 bytes).
                let mut message = BytesMut::with_capacity(12);
                message.put_u32(5);
                message.put_u8(4);
                message.put_u32(piece);

                message.freeze()
            }
            Message::Bitfield { bitfield } => {
                let bitfield = bitfield.as_bytes();
                let mut message = BytesMut::with_capacity(bitfield.len() + 5);

                message.put_u32((bitfield.len() + 1) as u32);
                message.put_u8(5);
                message.put_slice(&bitfield);

                message.freeze()
            }
            Message::Request { piece_idx, block_offset, block_length } => {
                let mut message = BytesMut::with_capacity(17);
                
                // Length (1 for Message ID + 4 for the Piece index + 8 for block offset and length).
                message.put_u32(13);
                message.put_u8(6);
                message.put_u32(piece_idx);
                message.put_u32(block_offset);
                message.put_u32(block_length);

                message.freeze()
            }
            Message::Piece { piece_idx, block_offset, block_data } => {
                let mut message = BytesMut::with_capacity(12 + block_data.len());

                // Length (1 for Message ID, 4 for piece idx, 4 for block offset, then block length).
                message.put_u32((9 + block_data.len()) as u32);
                message.put_u8(7);
                message.put_u32(piece_idx);
                message.put_u32(block_offset);
                message.put_slice(&block_data);

                message.freeze()
            }
            Message::Cancel { piece_idx, block_offset, block_length } => {
                let mut message = BytesMut::with_capacity(17);
                
                // Length (1 for Message ID + 4 for the Piece index + 8 for block offset and length).
                message.put_u32(13);
                message.put_u8(8);
                message.put_u32(piece_idx);
                message.put_u32(block_offset);
                message.put_u32(block_length);

                message.freeze()
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