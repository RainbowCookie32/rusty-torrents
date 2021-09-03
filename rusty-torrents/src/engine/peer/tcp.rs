use std::sync::Arc;
use std::net::SocketAddrV4;

use bytes::Buf;

use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use async_trait::async_trait;

use crate::engine::utils;
use crate::engine::TorrentInfo;
use crate::engine::peer::{Bitfield, Peer, PeerStatus, Message};

pub struct TcpPeer {
    address: SocketAddrV4,
    stream: Option<TcpStream>,

    status: PeerStatus,
    torrent_info: Arc<TorrentInfo>,

    bitfield_peer: Bitfield,
    
    requested: Option<usize>,
    waiting_for_response: bool
}

impl TcpPeer {
    pub async fn new(address: SocketAddrV4, torrent_info: Arc<TorrentInfo>) -> TcpPeer {
        let pieces = torrent_info.get_pieces_count().await;

        TcpPeer {
            address,
            stream: None,

            status: PeerStatus::new(),
            torrent_info,

            bitfield_peer: Bitfield::empty(pieces),

            requested: None,
            waiting_for_response: false
        }
    }

    fn get_handshake(&self) -> Vec<u8> {
        let id: String = vec!['e'; 20].iter().collect();
        let info_hash = self.torrent_info.data.info().info_hash();
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

    async fn get_peer_message(&mut self) -> Option<Message> {
        let mut length_buf = vec![0; 4];

        if let Some(stream) = self.stream.as_mut() {
            if let Ok(bytes) = stream.peek(&mut length_buf).await {
                if bytes == 4 {
                    let length = (length_buf.as_slice().get_u32() + 4) as usize;
                    let mut message_buffer = vec![0; length as usize];

                    if let Ok(bytes) = stream.peek(&mut message_buffer).await {
                        if bytes == length && stream.read_exact(&mut message_buffer).await.is_ok() {
                            return Message::from_data(message_buffer);
                        }
                    }
                }
            }
        }

        None
    }

    async fn send_peer_message(&mut self, message: Message) -> bool {
        match message {
            Message::KeepAlive => {}

            Message::Choke => {
                if let Some(stream) = self.stream.as_mut() {
                    let message = [0, 0, 0, 1, 0];

                    if let Err(e) = stream.write_all(&message).await {
                        println!("Error sending Choke message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                    else {
                        self.status.peer_choked = true;
                    }
                }
            }
            Message::Unchoke => {
                if let Some(stream) = self.stream.as_mut() {
                    let message = [0, 0, 0, 1, 1];

                    if let Err(e) = stream.write_all(&message).await {
                        println!("Error sending Unchoke message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                    else {
                        self.status.peer_choked = false;
                    }
                }
            }
            Message::Interested => {
                if let Some(stream) = self.stream.as_mut() {
                    let message = [0, 0, 0, 1, 2];

                    if let Err(e) = stream.write_all(&message).await {
                        println!("Error sending Interested message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                    else {
                        self.status.client_interested = true;
                    }
                }
            }
            Message::NotInterested => {
                if let Some(stream) = self.stream.as_mut() {
                    let message = [0, 0, 0, 1, 3];

                    if let Err(e) = stream.write_all(&message).await {
                        println!("Error sending Not Interested message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                    else {
                        self.status.client_interested = false;
                    }
                }
            }

            Message::Have(_data) => {}
            Message::Bitfield(data) => {
                if let Some(stream) = self.stream.as_mut() {
                    if let Err(e) = stream.write_all(&data).await {
                        println!("Error sending Bitfield message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                }
            }
            Message::Request(data) => {
                if let Some(stream) = self.stream.as_mut() {
                    if let Err(e) = stream.write_all(&data).await {
                        println!("Error sending Request message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                }
            }
            Message::Piece(data) => {
                if let Some(stream) = self.stream.as_mut() {
                    if let Err(e) = stream.write_all(&data).await {
                        println!("Error sending Piece message to peer {}. {}", self.address, e.to_string());
                        return false;
                    }
                }
            }
            Message::Cancel(_data) => {}
        }

        true
    }
}

#[async_trait]
impl Peer for TcpPeer {
    async fn connect(&mut self) -> bool {
        let handshake = self.get_handshake();
        self.stream = TcpStream::connect(&self.address).await.ok();

        if let Some(stream) = self.stream.as_mut() {
            if stream.write_all(&handshake).await.is_err() {
                return false;
            }
        }
        else {
            return false;
        }

        loop {
            if let Some(stream) = self.stream.as_mut() {
                if let Ok(bytes) = stream.peek(&mut [0; 68]).await {
                    if bytes >= 68 {
                        let mut buffer = vec![0; 68];
                        
                        if stream.read_exact(&mut buffer).await.is_ok() {
                            let mut data = Vec::new();
                            let mut bitfield = self.torrent_info.bitfield_client.read().await.as_bytes();
                            let message_length = bitfield.len() as u32 + 1;

                            data.append(&mut message_length.to_be_bytes().to_vec());
                            data.push(5);
                            data.append(&mut bitfield);

                            return self.send_peer_message(Message::Bitfield(data)).await;
                        }
                    }
                    else {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }

    async fn handle_peer_messages(&mut self) -> bool {
        if let Some(message) = self.get_peer_message().await {
            match message {
                Message::KeepAlive => {}

                Message::Choke => {
                    self.status.client_choked = true;
                }
                Message::Unchoke => {
                    self.status.client_choked = false;
                }
                Message::Interested => {
                    self.status.peer_interested = true;
                }
                Message::NotInterested => {
                    self.status.peer_interested = false;
                }

                Message::Have(data) => {
                    let piece = data.as_slice().get_u32();

                    self.bitfield_peer.piece_finished(piece);
                }
                Message::Bitfield(data) => {
                    self.bitfield_peer = Bitfield::from_peer_data(data);
                }
                Message::Request(data) => {
                    let mut data = data.as_slice();
                    
                    let piece_idx = data.get_u32();
                    let block_offset = data.get_u32() as usize;
                    let block_length = data.get_u32() as usize;

                    if self.torrent_info.bitfield_client.read().await.is_piece_available(piece_idx) && !self.status.client_choked {
                        let mut message = None;

                        if let Some(piece) = self.torrent_info.torrent_pieces.read().await.get(piece_idx as usize) {
                            let mut message_data = Vec::new();

                            let (start_file, start_position) = piece.get_offsets();
                            let piece = utils::read_piece(self.torrent_info.clone(), start_file, start_position).await;
                            let mut block_data = piece[block_offset..block_offset+block_length].to_vec();

                            // 9 = One byte for Message ID, 4 for Piece Index, and 4 for Block Offset.
                            let message_length = 9 + block_data.len() as u32;

                            message_data.append(&mut message_length.to_be_bytes().to_vec());
                            // ID 7 = Piece.
                            message_data.push(7);
                            message_data.append(&mut (block_offset as u32).to_be_bytes().to_vec());
                            message_data.append(&mut block_data);

                            message = Some(Message::Piece(message_data));
                        }

                        if let Some(message) = message {
                            if !self.send_peer_message(message).await {
                                return false;
                            }
                        }
                    }
                }
                Message::Piece(data) => {
                    let mut data_slice = data.as_slice();

                    let piece_idx = data_slice.get_u32();
                    let _block_offset = data_slice.get_u32();

                    if let Some(requested) = self.requested.as_ref() {
                        if *requested != piece_idx as usize {
                            // Mismatched piece, drop the peer.
                            println!("Received data for piece {}, but requested piece {}.", piece_idx, requested);
                            return false;
                        }
                    }
                    else {
                        // Unsolicited data, drop the peer just in case.
                        println!("Peer sent data for piece {}, but a piece wasn't requested.", piece_idx);
                        return false;
                    }

                    if let Some(piece) = self.torrent_info.torrent_pieces.write().await.get_mut(piece_idx as usize) {
                        let buf = data_slice.to_vec();

                        if piece.add_received_bytes(buf) {
                            if piece.check_piece() {
                                let (mut start_file, mut start_position) = piece.get_offsets();

                                piece.set_finished(true);
                                piece.set_requested(false);
                                utils::write_piece(self.torrent_info.clone(), piece.piece_data(), &mut start_file, &mut start_position).await;

                                println!("Piece {} finished.", piece_idx);
                            }
                            else {
                                piece.reset_piece();
                                println!("Piece {} was completed, but the hash didn't match.", piece_idx);
                            }
                        }
                    }

                    self.waiting_for_response = false;
                }
                Message::Cancel(data) => {
                    let mut data = data.as_slice();
                    
                    let _piece = data.get_u32();
                    let _block_offset = data.get_u32();
                    let _block_length = data.get_u32();
                }
            }
        }

        true
    }

    async fn handle_events(&mut self) {
        loop {
            self.handle_peer_messages().await;
            
            if !self.status.client_choked && !self.waiting_for_response {
                if self.requested.is_none() {
                    if !self.torrent_info.get_missing_pieces_count().await > 0 {
                        let idx = self.torrent_info.get_unfinished_piece_idx().await;

                        if self.bitfield_peer.is_piece_available(idx as u32) {
                            self.requested = Some(idx);
                            self.torrent_info.request_piece(idx).await;
                        }
                    }
                    else {
                        println!("No more missing pieces, exiting...");
                        break;
                    }
                }

                if self.requested.is_some() {
                    if self.status.peer_choked && !self.send_peer_message(Message::Unchoke).await {
                        break;
                    }

                    if !self.status.client_interested && !self.send_peer_message(Message::Interested).await {
                        break;
                    }

                    let wanted_piece = self.requested.unwrap();
                    let mut message = None;

                    if let Some(piece) = self.torrent_info.torrent_pieces.read().await.get(wanted_piece) {
                        if !piece.finished() {
                            let wanted_piece = wanted_piece as u32;
                            let (block_offset, block_length) = piece.get_block_request();

                            let length: u32 = 13;
                            let mut message_data = Vec::new();

                            // Length.
                            message_data.append(&mut length.to_be_bytes().to_vec());
                            // Message ID (6 for Request).
                            message_data.push(6);
                            // Message Body (Piece Index, Block Offset, and Block Length);
                            message_data.append(&mut wanted_piece.to_be_bytes().to_vec());
                            message_data.append(&mut block_offset.to_be_bytes().to_vec());
                            message_data.append(&mut block_length.to_be_bytes().to_vec());

                            self.requested = Some(wanted_piece as usize);
                            self.waiting_for_response = true;
                            
                            message = Some(Message::Request(message_data));
                        }
                        else {
                            self.requested = None;
                            self.waiting_for_response = false;
                        }
                    }

                    if let Some(message) = message {
                        if !self.send_peer_message(message).await {
                            break;
                        }
                    }
                }
            }

            tokio::task::yield_now().await;
        }

        // If a piece was requested by this peer, release it before dropping the connection
        // so another peer can complete it.
        if let Some(piece) = self.requested {
            self.torrent_info.torrent_pieces.write().await[piece].set_requested(false);
        }
        
        println!("Disconnecting from peer {}", self.address);
    }
}
