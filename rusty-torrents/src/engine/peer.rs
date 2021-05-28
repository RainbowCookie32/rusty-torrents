use std::net::SocketAddrV4;

use bytes::Buf;

use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub enum MessageKind {
    Choke,
    Unchoke,
    Interested,
    Uninterested,
    Have { piece_index: usize },
    Bitfield { available_pieces: Vec<u8> },
    Request { piece_index: usize, block_offset: usize, block_length: usize },
    Piece { piece_index: usize, block_offset: usize, block_data: Vec<u8> },
    Cancel { piece_index: usize, block_offset: usize, block_length: usize }
}

pub struct Peer {
    id: usize,

    address: SocketAddrV4,
    peer_stream: Option<TcpStream>,

    peer_interested: bool,
    peer_choking_client: bool,

    client_interested: bool,
    client_choking_peer: bool,

    waiting_for_response: bool,

    available_pieces: Vec<bool>,

    piece_data: Vec<u8>,
    requested_piece: Option<usize>
}

impl Peer {
    pub fn new(id: usize, address: SocketAddrV4, torrent_pieces: usize) -> Peer {
        let available_pieces = vec![false; torrent_pieces];

        Peer {
            id,
            address,
            peer_stream: None,

            peer_interested: false,
            peer_choking_client: true,

            client_interested: false,
            client_choking_peer: true,

            waiting_for_response: false,

            available_pieces,

            piece_data: Vec::new(),
            requested_piece: None,
        }
    }

    pub async fn connect_to_peer(&mut self, handshake: &[u8]) -> bool {
        if self.peer_stream.is_none() {
            let stream = TcpStream::connect(&self.address).await;

            if let Ok(peer_stream) = stream {
                self.peer_stream = Some(peer_stream);
                self.handshake_peer(handshake).await
            }
            else {
                println!("Error opening TcpStream to peer {}. Error: {}", self.address, stream.unwrap_err().to_string());
                false
            }
        }
        else {
            println!("Already connected to peer {}", self.address);
            true
        }
    }

    pub async fn handle_messages(&mut self) -> Option<MessageKind> {
        if let Some(message) = self.get_message().await {
            match &message {
                MessageKind::Choke => {
                    self.peer_choking_client = true;
                    None
                }
                MessageKind::Unchoke => {
                    self.peer_choking_client = false;
                    None
                }
                MessageKind::Interested => {
                    self.peer_interested = true;
                    None
                }
                MessageKind::Uninterested => {
                    self.peer_interested = false;
                    None
                }
                MessageKind::Have { piece_index } => {
                    if let Some(piece) = self.available_pieces.get_mut(*piece_index as usize) {
                        *piece = true;
                    }

                    None
                }
                MessageKind::Bitfield { available_pieces } => {
                    let mut current_piece = 0;

                    for byte in available_pieces {
                        for bit in 0..8 {
                            if let Some(piece) = self.available_pieces.get_mut(current_piece) {
                                let piece_available = (byte >> (7 - bit)) & 1 == 1;
                                *piece = piece_available;
                            }
                            
                            current_piece += 1;
                        }
                    }

                    None
                }
                // Other messages are handled in engine/mod.rs
                _ => Some(message)
            }
        }
        else {
            None
        }
    }

    pub async fn send_message(&mut self, message: MessageKind) {
        match message {
            MessageKind::Choke => {
                let message = [0, 0, 0, 1, 0];

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending choke message: {}", error.to_string());
                    }
                    else {
                        self.client_choking_peer = true;
                        println!("Peer {}: Sending choke message.", self.id);
                    }
                }
            }
            MessageKind::Unchoke => {
                let message = [0, 0, 0, 1, 1];

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending unchoke message: {}", error.to_string());
                    }
                    else {
                        self.client_choking_peer = false;
                        println!("Peer {}: Sending unchoke message.", self.id);
                    }
                }
            }
            MessageKind::Interested => {
                let message = [0, 0, 0, 1, 2];

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending interested message: {}", error.to_string());
                    }
                    else {
                        self.client_interested = true;
                        println!("Peer {}: Sending interested message.", self.id);
                    }
                }
            }
            MessageKind::Uninterested => {
                let message = [0, 0, 0, 1, 3];

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending uninterested message: {}", error.to_string());
                    }
                    else {
                        self.client_interested = false;
                        println!("Peer {}: Sending uninterested message.", self.id);
                    }
                }
            }
            MessageKind::Have { piece_index } => {
                let mut message = vec![0, 0, 0, 4, 4];
                let piece_index = piece_index.to_be_bytes();

                for byte in piece_index.iter() {
                    message.push(*byte);
                }

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending have message: {}", error.to_string());
                    }
                    else {
                        println!("Peer {}: Sending have message.", self.id);
                    }
                }
            }
            MessageKind::Bitfield { available_pieces } => {
                let mut message = Vec::with_capacity(available_pieces.len() + 5);
                let payload_length = (available_pieces.len() as u32).to_be_bytes();
                
                for byte in payload_length.iter() {
                    message.push(*byte);
                }

                message.push(5);

                for piece in available_pieces {
                    message.push(piece);
                }

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message) .await{
                        println!("Error sending bitfield message: {}", error.to_string());
                    }
                    else {
                        println!("Peer {}: Sending bitfield message.", self.id);
                    }
                }
            }
            MessageKind::Request { piece_index, block_offset, block_length } => {
                let b_piece_index = (piece_index as u32).to_be_bytes();
                let b_block_offset = (block_offset as u32).to_be_bytes();
                let b_block_length = (block_length as u32).to_be_bytes();

                let mut message = vec![0, 0, 0, 13, 6];

                for byte in b_piece_index.iter() {
                    message.push(*byte);
                }

                for byte in b_block_offset.iter() {
                    message.push(*byte);
                }

                for byte in b_block_length.iter() {
                    message.push(*byte);
                }

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending request message: {}", error.to_string());
                    }
                    else {
                        println!("Peer {}: Sending request message (piece: {}, block length: {}).", self.id, piece_index, block_length);
                    }
                }
            }
            MessageKind::Piece { piece_index, block_offset, block_data } => {
                let piece_index = piece_index.to_be_bytes();
                let block_offset = block_offset.to_be_bytes();
                let payload_length = (block_data.len() as u32).to_be_bytes();

                let mut message = Vec::with_capacity(block_data.len() + 5);
                
                for byte in payload_length.iter() {
                    message.push(*byte);
                }

                message.push(7);

                for byte in piece_index.iter() {
                    message.push(*byte);
                }

                for byte in block_offset.iter() {
                    message.push(*byte);
                }

                for piece in block_data {
                    message.push(piece);
                }

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending piece message: {}", error.to_string());
                    }
                    else {
                        println!("Peer {}: Sending piece message.", self.id);
                    }
                }
            }
            MessageKind::Cancel { piece_index, block_offset, block_length } => {
                let piece_index = piece_index.to_be_bytes();
                let block_offset = block_offset.to_be_bytes();
                let block_length = block_length.to_be_bytes();

                let mut message = vec![0, 0, 0, 12, 8];

                for byte in piece_index.iter() {
                    message.push(*byte);
                }

                for byte in block_offset.iter() {
                    message.push(*byte);
                }

                for byte in block_length.iter() {
                    message.push(*byte);
                }

                if let Some(socket) = self.peer_stream.as_mut() {
                    if let Err(error) = socket.write_all(&message).await {
                        println!("Error sending cancel message: {}", error.to_string());
                    }
                    else {
                        println!("Peer {}: Sending cancel message.", self.id);
                    }
                }
            }
        }
    }

    pub fn has_piece(&self, index: usize) -> bool {
        if let Some(piece) = self.available_pieces.get(index) {
            *piece
        }
        else {
            false
        }
    }

    async fn get_message(&mut self) -> Option<MessageKind> {
        if let Some(stream) = self.peer_stream.as_mut() {
            let mut length_buf = [0; 4];
            
            if let Ok(bytes) = stream.try_read(&mut length_buf) {
                if bytes == 4 {
                    let length = length_buf.as_ref().get_u32() as usize;
                    let mut message_buffer = vec![0; length];
    
                    if stream.read_exact(&mut message_buffer).await.is_ok() {
                        if length == 0 {
                            println!("Received keep-alive message");
                            return None;
                        }

                        match message_buffer.remove(0) {
                            0 => {
                                println!("Peer {}: Choke message received.", self.id);
                                return Some(MessageKind::Choke);
                            }
                            1 => {
                                println!("Peer {}: Unchoke message received.", self.id);
                                return Some(MessageKind::Unchoke);
                            }
                            2 => {
                                println!("Peer {}: Interested message received.", self.id);
                                return Some(MessageKind::Interested);
                            }
                            3 => {
                                println!("Peer {}: Uninterested message received.", self.id);
                                return Some(MessageKind::Uninterested);
                            }
                            4 => {
                                println!("Peer {}: Have message received.", self.id);
                                return Some(MessageKind::Have { piece_index: message_buffer.as_slice().get_u32() as usize });
                            }
                            5 => {
                                println!("Peer {}: Bitfield message received.", self.id);
                                return Some(MessageKind::Bitfield { available_pieces: message_buffer } );
                            }
                            6 => {
                                let mut buf = message_buffer.as_slice();
    
                                let piece_index = buf.get_u32() as usize;
                                let block_offset = buf.get_u32() as usize;
                                let block_length = buf.get_u32() as usize;
    
                                println!("Peer {}: Request message received (piece: {}, block length: {}).", self.id, piece_index, block_length);
                                return Some(MessageKind::Request { piece_index, block_offset, block_length });
                            }
                            7 => {
                                let mut buf = message_buffer.as_slice();
    
                                let piece_index = buf.get_u32() as usize;
                                let block_offset = buf.get_u32() as usize;
                                let block_data = buf.to_vec();
    
                                println!("Peer {}: Piece message received (piece: {}, block length: {}).", self.id, piece_index, block_data.len());
                                return Some(MessageKind::Piece { piece_index, block_offset, block_data });
                            }
                            8 => {
                                let mut buf = message_buffer.as_slice();
    
                                let piece_index = buf.get_u32() as usize;
                                let block_offset = buf.get_u32() as usize;
                                let block_length = buf.get_u32() as usize;
    
                                println!("Peer {}: Cancel message received.", self.id);
                                return Some(MessageKind::Cancel { piece_index, block_offset, block_length });
                            }
                            _ => {
                                println!("Unknown message received {}.", message_buffer[0]);
                            }
                        }
                    }
                }
            }
        }

        None
    }

    async fn handshake_peer(&mut self, handshake: &[u8]) -> bool {
        if let Some(peer_stream) = self.peer_stream.as_mut() {
            if let Ok(result) = peer_stream.write(&handshake).await {
                if result == handshake.len() {
                    let mut response = vec![0; 68];
                    let read_result = peer_stream.read_exact(&mut response).await;
    
                    if read_result.is_ok() {
                        println!("Successful handshake with peer at {}.", self.address);
                        true
                    }
                    else {
                        println!("Failed to receive response from peer at {}. Error: {}", self.address, read_result.unwrap_err().to_string());
                        false
                    }
                }
                else {
                    println!("Couldn't transfer all the handshake to peer at {}", self.address);
                    false
                }
            }
            else {
                println!("Couldn't handshake peer at {}", self.address);
                false
            }
        }
        else {
            println!("Tried to handshake peer {} without an active TcpStream", self.address);
            false
        }
    }

    pub fn peer_choking_client(&self) -> bool {
        self.peer_choking_client
    }

    pub fn client_interested(&self) -> bool {
        self.client_interested
    }

    pub fn client_choking_peer(&self) -> bool {
        self.client_choking_peer
    }

    /// Get a reference to the peer's requested piece.
    pub fn requested_piece(&self) -> &Option<usize> {
        &self.requested_piece
    }

    pub fn set_requested_piece(&mut self, index: usize) {
        self.requested_piece = Some(index);
    }

    pub fn waiting_for_response(&self) -> bool {
        self.waiting_for_response
    }

    pub fn set_waiting_for_response(&mut self, waiting_for_response: bool) {
        self.waiting_for_response = waiting_for_response;
    }

    /// Get a reference to the peer's piece data.
    pub fn piece_data(&self) -> &Vec<u8> {
        &self.piece_data
    }

    /// Get a mutable reference to the peer's piece data.
    pub fn piece_data_mut(&mut self) -> &mut Vec<u8> {
        &mut self.piece_data
    }

    pub fn piece_finished(&mut self) -> Vec<u8> {
        let data = self.piece_data.clone();

        self.piece_data = Vec::new();
        self.requested_piece = None;

        data
    }
}
