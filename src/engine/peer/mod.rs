pub mod message;

use std::sync::Arc;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use bytes::Buf;
use tokio::time;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::io::{AsyncWriteExt, AsyncReadExt};

use message::Message;

type CmdRx = mpsc::UnboundedReceiver<PeerCommand>;
type StatusTx = watch::Sender<PeerStatus>;
type PieceRx = broadcast::Receiver<usize>;
type PieceDataTx = mpsc::UnboundedSender<(usize, Vec<u8>)>;

const PROTOCOL: [u8; 19] = *b"BitTorrent protocol";
const PLACEHOLDER_ID: [u8; 20] = *b"00000000000000000000";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerStatus {
    /// Waiting for the peer to reply to a message or unchoke us.
    Waiting,
    /// The peer was dropped. This can be caused by the peer sending bad data,
    /// or being unresponsive/choked for too long.
    Dropped,
    /// Connected successfully to the peer and got Bitfield, but waiting for Unchoke.
    Connected { available_pieces: Vec<bool> },
    /// Peer unchoked us.
    Available { available_pieces: Vec<bool> },
}

#[derive(Debug)]
pub enum PeerCommand {
    Disconnect,
    SendInterested,
    RequestPiece(usize, u64)
}

pub struct TcpPeer {
    stream: TcpStream,
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

    /// Holds the Piece data received from the Peer.
    piece_data: Vec<u8>,
    /// The index and length of the requested Piece.
    piece_info: Option<(usize, u64)>,

    /// Gets commands from the Engine, like Piece requests,
    /// or disconnects.
    cmd_rx: mpsc::UnboundedReceiver<PeerCommand>,
    /// Sends info about the Peer's status to the Engine for work coordination.
    /// This updates are connection and choke state, and available pieces.
    peer_status_tx: watch::Sender<PeerStatus>,

    /// Receives info about completed pieces from the Engine.
    complete_piece_rx: broadcast::Receiver<usize>,
    /// Sends the index and data of a complete Piece back to the Engine
    /// for checking and writing.
    complete_piece_data_tx: mpsc::UnboundedSender<(usize, Vec<u8>)>,

    /// The amount of time the peer had us choked for.
    choked_since: Option<Instant>,
    /// The amount of time we've been waiting for the peer to reply.
    waiting_since: Option<Instant>,
    /// The amount of time that passed since the Piece assigned.
    time_since_assign: Option<Instant>
}

impl TcpPeer {
    // Gets the address of the Peer and attemps to create a TcpStream to it.
    // Connection attempts should only happen after checking the torrent!
    pub async fn connect(
        address: SocketAddrV4,
        info_hash: Arc<[u8; 20]>,
        completed_pieces: Vec<bool>,
        cmd_rx: CmdRx,
        peer_status_tx: StatusTx,
        complete_piece_rx: PieceRx,
        complete_piece_data_tx: PieceDataTx
    ) -> Option<TcpPeer> {
        let stream = TcpStream::connect(address).await.ok()?;

        Some(
            TcpPeer {
                stream,
                info_hash,
    
                peer_interested: false,
                client_interested: false,
    
                peer_choking: true,
                client_choking: true,
    
                available_pieces: Vec::new(),
                completed_pieces,

                piece_data: Vec::new(),
                piece_info: None,
    
                cmd_rx,
                peer_status_tx,
    
                complete_piece_rx,
                complete_piece_data_tx,
    
                choked_since: None,
                waiting_since: None,
                time_since_assign: None,
            }
        )
    }

    /// Starts communication with the peer. Sends the handshake
    /// and receives/sends messages about the active torrent.
    pub async fn connect_to_peer(mut self) {
        if !self.send_handshake().await {
            self.update_peer_status(PeerStatus::Dropped);
            return;
        }

        if !self.send_message(Message::Bitfield { bitfield: Bitfield::from_pieces(self.completed_pieces.clone()) }).await {
            self.update_peer_status(PeerStatus::Dropped);
            return;
        }

        self.update_peer_status(PeerStatus::Waiting);

        loop {
            if self.handle_engine_cmd().await {
                break;
            }

            if let Ok(piece) = self.complete_piece_rx.try_recv() {
                if !self.send_message(Message::Have { piece: piece as u32 }).await {
                    // Error sending message, drop.
                    break;
                }
            }

            if let Some(time_choked) = self.choked_since.as_ref() {
                if time_choked.elapsed() > Duration::from_secs(300) {
                    // If we spent 5 minutes choked, it's probably time to find another peer.
                    break;
                }
            }

            if let Some(time_waiting) = self.waiting_since.as_ref() {
                if time_waiting.elapsed() > Duration::from_secs(30) {
                    // 30 whole seconds waiting for a Piece. yeet.
                    break;
                }
            }
            else if let Some((idx, size)) = self.piece_info.as_ref() {
                let piece_idx = *idx as u32;
                let block_offset = self.piece_data.len() as u32;
                let block_length = (*size - self.piece_data.len() as u64).min(16384) as u32;

                let message = Message::Request { piece_idx, block_offset, block_length };

                self.update_peer_status(PeerStatus::Waiting);

                if self.send_message(message).await {
                    self.waiting_since = Some(Instant::now());
                }
                else {
                    break;
                }
            }

            if let Some(time_since_assign) = self.time_since_assign.as_ref() {
                // Pieces are usually small (biggest I've seen so far was 1.5MB).
                // If we can't get a whole piece from a Peer in less than 60 seconds,
                // then we might have a pretty slow peer on our hands.
                if time_since_assign.elapsed() > Duration::from_secs(60) {
                    println!("slow peer detected, dropping...");
                    break;
                }
            }

            if let Some(msg) = self.get_message().await {
                if self.piece_info.is_none() {
                    if self.peer_choking {
                        self.update_peer_status(PeerStatus::Connected { available_pieces: self.available_pieces.clone() });
                    }
                    else {
                        self.update_peer_status(PeerStatus::Available { available_pieces: self.available_pieces.clone() });
                    }
                }

                match msg {
                    Message::KeepAlive => {}
                    Message::Choke => {
                        self.peer_choking = true;
                        self.choked_since = Some(Instant::now());

                        self.update_peer_status(PeerStatus::Waiting);
                    }
                    Message::Unchoke => {
                        // Some peers send a second Unchoke message after requesting a piece.
                        // Only update status if we are actually chaging from Choked to Unchoked.
                        if self.peer_choking {
                            self.peer_choking = false;
                            self.choked_since = None;

                            if !self.available_pieces.is_empty() {
                                self.update_peer_status(PeerStatus::Available { available_pieces: self.available_pieces.clone() });
                            }
                        }
                    }
                    Message::Interested => {
                        self.peer_interested = true;
                        
                        if self.client_choking {
                            self.client_choking = false;
                            
                            if !self.send_message(Message::Unchoke).await {
                                self.update_peer_status(PeerStatus::Dropped);
                                break;
                            }
                        }
                    }
                    Message::NotInterested => {
                        self.peer_interested = false;

                        if !self.client_choking {
                            self.client_choking = true;

                            if !self.send_message(Message::Choke).await {
                                self.update_peer_status(PeerStatus::Dropped);
                                break;
                            }
                        }
                    }
                    Message::Have { piece } => {
                        let piece = piece as usize;

                        if piece >= self.available_pieces.len() {
                            break;
                        }
                        
                        self.available_pieces[piece] = true;
                    }
                    Message::Bitfield { bitfield } => {
                        self.available_pieces = bitfield.pieces;
                    }
                    Message::Request { .. } => todo!(),
                    Message::Piece { piece_idx, mut block_data, .. } => {
                        if let Some((requested, size)) = self.piece_info.as_ref() {
                            if piece_idx as usize == *requested {
                                self.piece_data.append(&mut block_data);

                                if self.piece_data.len() == *size as usize {
                                    self.complete_piece_data_tx.send((*requested, self.piece_data))
                                        .expect("Failed to send Piece data to Engine")
                                    ;

                                    self.piece_info = None;
                                    self.piece_data = Vec::new();
                                    self.time_since_assign = None;

                                    self.update_peer_status(PeerStatus::Available { available_pieces: self.available_pieces.clone() });
                                }
                            }
                            else {
                                // Peer sent the wrong piece piece, drop.
                                break;
                            }
                        }
                        else {
                            // Peer sent an unsolicited piece, drop.
                            break;
                        }

                        self.waiting_since = None;
                    }
                    Message::Cancel { .. } => todo!(),
                }
            }

            time::sleep(Duration::from_millis(5)).await;
        }

        self.update_peer_status(PeerStatus::Dropped);

        if let Some((piece, size)) = self.piece_info {
            let piece_idx = piece as u32;
            let block_offset = self.piece_data.len() as u32;
            let block_length = (size - self.piece_data.len() as u64).min(16384) as u32;

            let message = Message::Cancel {
                piece_idx,
                block_offset,
                block_length
            };

            self.send_message(message).await;
        }
    }

    /// Handles commands sent by the engine, returns true if it should drop the peer.
    async fn handle_engine_cmd(&mut self) -> bool {
        if let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                PeerCommand::Disconnect => return true,
                PeerCommand::SendInterested => {
                    self.client_choking = false;
                    self.client_interested = true;

                    if self.client_choking && !self.send_message(Message::Unchoke).await {
                        // Error sending message, drop.
                        return true;
                    }

                    if !self.client_interested && !self.send_message(Message::Interested).await {
                        // Error sending message, drop.
                        return true;
                    }
                }
                PeerCommand::RequestPiece(idx, size) => {
                    self.piece_info = Some((idx, size));
                    self.piece_data = Vec::with_capacity(size as usize);
                    self.time_since_assign = Some(Instant::now());
                }
            }
        }

        false
    }

    /// Try to get a message from the TcpStream if available.
    async fn get_message(&mut self) -> Option<Message> {
        let mut message = None;
        let mut length_buf = vec![0; 4];

        let read_bytes = self.stream.try_read(&mut length_buf).ok()?;

        if read_bytes == 4 {
            let msg_length = length_buf.as_slice().get_u32() as usize;
            let mut msg_buf = vec![0; msg_length];

            let read_bytes = self.stream.read_exact(&mut msg_buf).await.ok()?;

            if read_bytes == msg_length {
                message = Some(msg_buf.into());
            }
        }

        message
    }

    /// Try to send a message through the TcpStream. Returns false
    /// if the message couldn't be sent.
    async fn send_message(&mut self, message: Message) -> bool {
        let msg_buf: Vec<u8> = message.into();

        if let Ok(result) = time::timeout(Duration::from_secs(30), self.stream.write_all(&msg_buf)).await {
            result.is_ok()
        }
        else {
            false
        }
    }

    async fn send_handshake(&mut self) -> bool {
        let mut handshake = Vec::with_capacity(68);
        handshake.push(19);

        // First 20 bytes of the handshake are used by the
        // length-prefixed "BitTorrent protocol" string,
        for protocol_byte in PROTOCOL.iter() {
            handshake.push(*protocol_byte);
        }

        // then 8 reserved bytes (used for extension i think),
        handshake.resize(handshake.len() + 8, 0);

        // and finally, the info hash.
        for info_hash_byte in self.info_hash.iter() {
            handshake.push(*info_hash_byte);
        }

        // And finally, the peer id.
        for peer_id_byte in PLACEHOLDER_ID.iter() {
            handshake.push(*peer_id_byte);
        }

        self.update_peer_status(PeerStatus::Waiting);

        let send_timeout = time::timeout(
            Duration::from_secs(5),
            self.stream.write_all(&handshake)
        ).await;

        if send_timeout.is_ok() {
            let mut response_buf = vec![0; 68];
            let receive_timeout = time::timeout(
                Duration::from_secs(5),
                self.stream.read(&mut response_buf)
            ).await;

            if let Ok(timeout_result) = receive_timeout {
                if let Ok(bytes_read) = timeout_result {
                    // TODO: Should probably check peer id and info hash.
                    return bytes_read == response_buf.len();
                }
            }
        }
        
        false
    }

    fn update_peer_status(&mut self, status: PeerStatus) {
        // Only send Available messages when we don't have a Piece already assigned.
        // This causes Engine to assign another and have the old Piece get stuck in limbo.
        if let PeerStatus::Available { .. } = &status {
            if self.piece_info.is_none() {
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
