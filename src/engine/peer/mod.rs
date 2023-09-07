pub mod message;

use std::sync::Arc;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::*;
use tokio::time;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::io::{AsyncWriteExt, AsyncReadExt};

use message::Message;

type CmdRx = mpsc::UnboundedReceiver<PeerCommand>;
type StatusTx = watch::Sender<PeerStatus>;
type PieceRx = broadcast::Receiver<usize>;
type PieceDataTx = mpsc::UnboundedSender<(usize, Vec<u8>)>;
type AvailablePiecesTx = mpsc::UnboundedSender<Vec<bool>>;

const PROTOCOL: [u8; 19] = *b"BitTorrent protocol";
const PLACEHOLDER_ID: [u8; 20] = *b"00000000000000000000";

// This can slow everything down a fair bit, but it's useful for debugging.
const LOG_ALL_MESSAGES: bool = false;

struct PieceRequest {
    idx: usize,
    size: usize,
    data: Vec<u8>,

    block_offset: u32,
    block_length: u32,
    received_data: usize
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerStatus {
    /// Waiting for Unchoke/still has pieces on queue to request.
    Busy,
    /// Ready to request pieces from the peer.
    /// Signals the engine it can assign pieces to this peer.
    Ready,
    /// The peer was disconnected for whatever reason.
    Disconnected,
    /// The peer requested a piece from us.
    RequestedPiece(usize),
    /// Handshakes and Bitfields were exchanged.
    /// The engine can evaluate whether the peer is relevant.
    ConnectionEstablished,
}

#[derive(Debug)]
pub enum PeerCommand {
    Disconnect,
    SendInterested,
    SendNotInterested,

    SendPiece { piece: usize, data: Vec<u8> },
    RequestPiece { pieces: Vec<(usize, u64)> }
}

pub struct TcpPeer {
    stream: TcpStream,
    address: SocketAddr,

    /// SHA-1 hash of the info section of the torrent file.
    info_hash: Arc<[u8; 20]>,

    /// The peer is interested on us, might request a Piece.
    peer_interested: bool,
    /// We are interested on the Peer, might request a Piece.
    client_interested: bool,

    /// Peer is choking us, can't send any requests.
    peer_choking: bool,
    /// We are choking the peer, ignore any requests.
    client_choking: bool,

    /// Each entry on this Vec represents whether or not a piece
    /// is available on this peer. It's calculated from the Bitfield message
    /// and updated when receiving Have messages.
    available_pieces: Vec<bool>,
    /// The pieces *we* have that are complete.
    completed_pieces: Vec<bool>,

    /// The piece that was requested by the Peer.
    peer_request: Option<PieceRequest>,
    /// Whether we still have to send a block of data.
    peer_request_pending: bool,

    /// The piece we requested to the Peer.
    client_request: Option<PieceRequest>,
    /// The next pieces to request.
    client_request_queue: Vec<PieceRequest>,

    /// Gets commands from the Engine, like Piece requests,
    /// or disconnects.
    cmd_rx: CmdRx,
    /// Sends info about the Peer's status to the Engine for work coordination.
    /// This updates are connection and choke state, and available pieces.
    peer_status_tx: StatusTx,

    /// Receives info about completed pieces from the Engine.
    complete_piece_rx: PieceRx,
    /// Sends the index and data of a complete Piece back to the Engine
    /// for checking and writing.
    complete_piece_data_tx: PieceDataTx,
    /// Sends info to the engine about pieces available from this peer.
    available_pieces_tx: AvailablePiecesTx,

    /// true if we sent a message and are waiting for the reply.
    waiting_for_message: bool,
    /// true if we are waiting for a piece message specifically.
    waiting_for_piece_data: bool,

    unknown_messages_received: u8,
}

impl TcpPeer {
    // Gets the address of the Peer and attemps to create a TcpStream to it.
    // Connection attempts should only happen after checking the torrent!
    pub async fn connect(
        address: SocketAddr,
        info_hash: Arc<[u8; 20]>,
        completed_pieces: Vec<bool>,
        cmd_rx: CmdRx,
        peer_status_tx: StatusTx,
        complete_piece_rx: PieceRx,
        complete_piece_data_tx: PieceDataTx,
        available_pieces_tx: AvailablePiecesTx
    ) -> Option<TcpPeer> {
        let stream = TcpStream::connect(&address).await.ok()?;

        Some(
            TcpPeer {
                stream,
                address,

                info_hash,
    
                peer_interested: false,
                client_interested: false,
    
                peer_choking: true,
                client_choking: true,
    
                available_pieces: Vec::new(),
                completed_pieces,

                peer_request: None,
                peer_request_pending: false,
                
                client_request: None,
                client_request_queue: Vec::new(),
    
                cmd_rx,
                peer_status_tx,
    
                complete_piece_rx,
                complete_piece_data_tx,
                available_pieces_tx,
    
                waiting_for_message: false,
                waiting_for_piece_data: false,

                unknown_messages_received: 0,
            }
        )
    }

    /// Starts communication with the peer. Sends the handshake
    /// and receives/sends messages about the active torrent.
    pub async fn run(mut self) {
        if !self.establish_connection().await {
            println!("[{}]: failed to establish connection, dropping.", self.address);
            self.update_peer_status(PeerStatus::Disconnected);
            return;
        }
        else {
            println!("[{}]: Connection established!", self.address);
        }

        loop {
            // Check if engine sent any commands and handle them.
            if self.handle_engine_cmd().await {
                break;
            }

            // Notify the peer if we have any new pieces.
            if let Ok(piece) = self.complete_piece_rx.try_recv() {
                if !self.send_message(Message::Have { piece: piece as u32 }).await {
                    // Error sending message, drop.
                    break;
                }
            }

            if self.client_interested && self.peer_choking {
                self.waiting_for_message = true;
            }

            if self.client_interested && !self.peer_choking && self.client_request.is_none() && self.client_request_queue.is_empty() {
                self.update_peer_status(PeerStatus::Ready);
            }

            if self.peer_request_pending && self.peer_request.is_some() {
                if !self.send_peer_request().await {
                    break;
                }
            }

            if self.waiting_for_message || self.waiting_for_piece_data {
                if let Some(msg) = self.get_message().await {
                    if !self.handle_peer_msg(msg).await {
                        break;
                    }
    
                    self.waiting_for_message = false;
                }
            }
            else {
                // Send a Piece request if we have one pending.
                if !self.peer_choking && self.client_request.is_some() && !self.waiting_for_piece_data {
                    if !self.send_client_request().await {
                        break;
                    }
                }

                time::sleep(Duration::from_millis(5)).await;
            }
        }

        self.update_peer_status(PeerStatus::Disconnected);

        if let Some(piece) = self.client_request.take() {
            let piece_idx = piece.idx as u32;
            let block_offset = piece.data.len() as u32;
            let block_length = (piece.size - piece.data.len()).min(16384) as u32;

            let message = Message::Cancel {
                piece_idx,
                block_offset,
                block_length
            };

            self.send_message(message).await;
        }
    }

    async fn establish_connection(&mut self) -> bool {
        if !self.send_handshake().await {
            return false;
        }

        if !self.send_message(Message::Bitfield { bitfield: Bitfield::from_pieces(self.completed_pieces.clone()) }).await {
            return false;
        }

        self.waiting_for_message = true;
        true
    }

    async fn handle_peer_msg(&mut self, msg: Message) -> bool {
        match msg {
            Message::KeepAlive => {}
            Message::Choke => {
                self.peer_choking = true;
                self.update_peer_status(PeerStatus::Busy);
            }
            Message::Unchoke => {
                // Some peers send a second Unchoke message after requesting a piece.
                // Only update status if we are actually chaging from Choked to Unchoked.
                if self.peer_choking {
                    self.peer_choking = false;

                    if !self.available_pieces.is_empty() {
                        self.update_peer_status(PeerStatus::Ready);
                    }
                }
            }
            Message::Interested => {
                self.peer_interested = true;
                
                if self.client_choking {
                    self.client_choking = false;
                    
                    if !self.send_message(Message::Unchoke).await {
                        return false;
                    }
                }
            }
            Message::NotInterested => {
                self.peer_interested = false;
            }
            Message::Have { piece } => {
                let piece = piece as usize;

                if piece >= self.available_pieces.len() {
                    println!("[{}]: peer sent have message for an invalid piece", self.address);
                    return false;
                }
                
                self.available_pieces[piece] = true;
                self.update_peer_pieces();
            }
            Message::Bitfield { bitfield } => {
                self.available_pieces = bitfield.pieces;
                self.update_peer_pieces();
                self.update_peer_status(PeerStatus::ConnectionEstablished);
            }
            Message::Request { piece_idx, block_offset, block_length } => {
                let piece_idx = piece_idx as usize;

                if let Some(piece) = self.peer_request.as_mut() {
                    piece.block_offset = block_offset;
                    piece.block_length = block_length;

                    if piece.idx != piece_idx {
                        piece.idx = piece_idx;
                        
                        piece.data.clear();
                    }
                }
                else {
                    let request = PieceRequest {
                        idx: piece_idx,
                        size: 0,
                        data: Vec::new(),

                        block_offset,
                        block_length,
                        received_data: 0
                    };

                    self.peer_request = Some(request);
                }

                self.peer_request_pending = true;
                self.update_peer_status(PeerStatus::RequestedPiece(piece_idx));
            }
            Message::Piece { piece_idx, block_offset, block_data } => {
                if let Some(piece) = self.client_request.as_mut() {
                    if piece_idx as usize == piece.idx {
                        let block_offset = block_offset as usize;
                        piece.received_data += block_data.len();

                        for (i, byte) in block_data.into_iter().enumerate() {
                            piece.data[block_offset + i] = byte;
                        }

                        if piece.received_data == piece.size {
                            println!("[{}]: completed piece {piece_idx}", self.address);

                            self.complete_piece_data_tx.send((piece.idx, piece.data.clone()))
                                .expect("Failed to send Piece data to Engine")
                            ;

                            self.client_request = self.client_request_queue.pop();
                            self.waiting_for_piece_data = false;
                        }
                    }
                    else {
                        // Peer sent the wrong piece piece, drop.
                        println!("[{}]: peer sent the wrong piece (requested {}, received {piece_idx})", self.address, piece.idx);
                        return false;
                    }
                }
                else {
                    // Peer sent an unsolicited piece, drop.
                    println!("[{}]: peer sent a piece we didn't request", self.address);
                    return false;
                }
            }
            Message::Cancel { .. } => {
                self.client_request = None;
            }
            Message::Unknown(id) => {
                println!("[{}]: peer sent an unknown message id ({id})", self.address);
                self.unknown_messages_received += 1;

                if self.unknown_messages_received >= 5 {
                    println!("[{}]: peer sent too many unknown messages, dropping it.", self.address);
                    return false;
                }
            }
        }

        true
    }

    /// Handles commands sent by the engine, returns true if it should drop the peer.
    async fn handle_engine_cmd(&mut self) -> bool {
        if let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PeerCommand::Disconnect => return true,
                PeerCommand::SendInterested => {
                    if self.client_choking && !self.send_message(Message::Unchoke).await {
                        println!("[{}]: failed to send unchoke message to peer", self.address);
                        // Error sending message, drop.
                        return true;
                    }

                    self.client_choking = false;

                    if !self.client_interested && !self.send_message(Message::Interested).await {
                        println!("[{}]: failed to send interested message to peer", self.address);
                        // Error sending message, drop.
                        return true;
                    }

                    self.client_interested = true;
                }
                PeerCommand::SendNotInterested => {
                    if !self.send_message(Message::NotInterested).await {
                        println!("[{}]: failed to send not interested message to peer", self.address);
                        // Error sending message, drop.
                        return true;
                    }
                }
                PeerCommand::SendPiece { piece, data } => {
                    if let Some(request) = self.peer_request.as_mut() {
                        if request.idx == piece {
                            request.data = data;
                            self.update_peer_status(PeerStatus::Busy);
                        }
                    }
                }
                PeerCommand::RequestPiece{ pieces } => {
                    let mut pieces = pieces.into_iter()
                        .rev()
                        .map(| (piece, length) | {
                            PieceRequest {
                                idx: piece,
                                size: length as usize,
                                data: vec![0; length as usize],
        
                                block_offset: 0,
                                block_length: 0,
                                received_data: 0,
                            }
                        })
                        .collect::<Vec<PieceRequest>>()
                    ;

                    if self.client_request.is_none() {
                        self.client_request = pieces.pop();
                    }

                    self.update_peer_status(PeerStatus::Busy);
                    self.client_request_queue.append(&mut pieces);
                }
            }
        }

        false
    }

    /// Try to get a message from the TcpStream if available.
    async fn get_message(&mut self) -> Option<Message> {
        let mut message = None;
        let mut length_buf = vec![0; 4];

        let read_bytes = self.stream.read(&mut length_buf).await.ok()?;

        if read_bytes == 4 {
            let msg_length = length_buf.as_slice().get_u32() as usize;

            if msg_length > 65535 {
                return None;
            }

            let mut msg_buf = vec![0; msg_length];

            let read_bytes = self.stream.read_exact(&mut msg_buf).await.ok()?;

            if read_bytes == msg_length {
                let msg_buf = Bytes::from(msg_buf);
                let msg = msg_buf.into();

                if LOG_ALL_MESSAGES {
                    println!("[{}]: Received message {msg}", self.address);
                }

                message = Some(msg);
            }
        }

        message
    }

    /// Try to send a message through the TcpStream. Returns false
    /// if the message couldn't be sent.
    async fn send_message(&mut self, message: Message) -> bool {
        if LOG_ALL_MESSAGES {
            println!("[{}]: Sent message {message}", self.address);
        }

        let msg_buf: Bytes = message.into();
        match self.stream.write_all(&msg_buf).await {
            Ok(_) => true,
            Err(e) => {
                println!("error sending message: {e}");
                false
            }
        }
    }

    async fn send_handshake(&mut self) -> bool {
        let mut handshake = BytesMut::with_capacity(68);
        
        // Length prefix of "BitTorrent protocol" string.
        handshake.put_u8(19);
        // The aforementioned string.
        handshake.put_slice(&PROTOCOL);
        // 8 reserved bytes, used for extensions.
        // TODO: Start implementing extensions, they have cool stuff.
        handshake.put_u64(0);
        // The torrent's info hash.
        handshake.put_slice(self.info_hash.as_ref());
        // Our peer id.
        handshake.put_slice(&PLACEHOLDER_ID);

        if self.stream.write_all(&handshake).await.is_ok() {
            let mut response_buf = vec![0; 68];

            if let Ok(bytes_read) = self.stream.read(&mut response_buf).await {
                // Didn't get a complete handshake, yeet.
                if bytes_read != response_buf.len() {
                    return false;
                }

                let mut response_bytes = Bytes::from(response_buf);

                // Sanity check for the length prefix.
                if response_bytes.get_u8() != 19 {
                    return false;
                }

                // Skip the string, too lazy.
                response_bytes.advance(19);

                // The reserved bytes for extensions.
                // TODO: Check them and do stuff.
                let _reserved_bytes = response_bytes.get_u64();
                
                return true;
            }
        }
        
        false
    }

    async fn send_client_request(&mut self) -> bool {
        let mut messages = Vec::new();
        let piece = self.client_request.as_mut().unwrap();
        let piece_idx = piece.idx as u32;

        loop {
            let block_offset = piece.block_offset;
            let pending_to_request = piece.size as u32 - block_offset;

            if pending_to_request == 0 {
                break;
            }

            let block_length = pending_to_request.min(16384) as u32;
            let message = Message::Request { piece_idx, block_offset, block_length };

            messages.push(message);
            piece.block_offset += block_length;
        }

        for message in messages {
            if !self.send_message(message).await {
                println!("[{}]: failed to send request message to peer", self.address);
                return false;
            }
        }

        self.waiting_for_piece_data = true;
        true
    }

    async fn send_peer_request(&mut self) -> bool {
        let piece = self.peer_request.as_ref().unwrap();

        if !piece.data.is_empty() {
            let piece_idx = piece.idx as u32;
            let block_length = piece.block_length as usize;
            let block_start = piece.block_offset as usize;
            let block_end = block_start + block_length;
            let block_data = piece.data[block_start..block_end].to_vec();

            assert!(block_data.len() == block_length);
            println!("[{}]: sending piece data to {} (idx: {piece_idx}, offset: {block_start}, length: {block_length})", self.address, self.stream.peer_addr().unwrap());

            let message = Message::Piece {
                piece_idx,
                block_offset: block_start as u32,
                block_data
            };

            return self.send_message(message).await;
        }

        false
    }

    fn update_peer_pieces(&mut self) {
        self.available_pieces_tx.send(self.available_pieces.clone())
            .expect("Failed to send available_pieces to engine");
    }

    fn update_peer_status(&mut self, status: PeerStatus) {
        if status == PeerStatus::Ready {
            if self.client_request_queue.is_empty() {
                self.peer_status_tx.send(status)
                    .expect("Failed to communicate with Engine");
            }
        }
        else {
            self.peer_status_tx.send(status)
                .expect("Failed to communicate with Engine");
        }
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

    pub fn from_pieces(pieces: Vec<bool>) -> Bitfield {
        Bitfield {
            pieces
        }
    }

    pub fn from_peer_data(data: &[u8]) -> Bitfield {
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
