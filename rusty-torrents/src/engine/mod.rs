mod file;
mod peer;
mod piece;

use std::sync::Arc;
use std::net::{Ipv4Addr, SocketAddrV4};

use sha1::Sha1;
use bytes::Buf;
use rusty_parser::{BEncodeType, Torrent};

use tokio::task;
use tokio::sync::RwLock;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use file::File;
use piece::Piece;
use peer::{Peer, MessageKind};

pub struct TorrentInfo {
    data: Torrent,
    piece_length: usize,
    handshake_data: Vec<u8>,

    files: Arc<RwLock<Vec<File>>>,
    pieces: Arc<RwLock<Vec<Piece>>>,
    hashes: Arc<Vec<[u8; 20]>>,

    bitfield: Arc<RwLock<Vec<u8>>>,
}

impl TorrentInfo {

}

pub struct Engine {
    client_id: String,
    torrent_info: Arc<TorrentInfo>
}

impl Engine {
    pub async fn init(data: Vec<u8>) -> Engine {
        let data = Torrent::new(data).unwrap();
        let (info, info_hash) = data.info();

        let client_id: String = vec!['r'; 20].iter().collect();
        let handshake_data = Engine::get_handshake(&client_id, &info_hash);
        let piece_length = info.get("piece length").unwrap().get_int() as usize;
        
        let bitfield = Arc::new(RwLock::new(Vec::new()));
        let files = Arc::new(RwLock::new(Engine::build_file_list(data.get_files(), piece_length as usize).await));
        let pieces = Arc::new(RwLock::new(Vec::new()));
        let hashes = Arc::new(Engine::build_hashes_list(info.get("pieces").unwrap().get_string_bytes()).await);

        println!("Torrent name: {}", info.get("name").unwrap().get_string());

        let torrent_info = Arc::new(TorrentInfo {
            data,
            piece_length,
            handshake_data,

            files,
            pieces,
            hashes,

            bitfield
        });

        Engine {
            client_id,
            torrent_info
        }
    }

    fn update_peer_list(&self, body: Vec<u8>) -> Vec<Peer> {
        let mut result = Vec::new();

        if let Ok(response_data) = BEncodeType::dictionary(body.as_ref(), &mut 1) {
            let entries = response_data.get_dictionary();
            let peers_bytes = entries.get("peers").unwrap().get_string_bytes();

            let mut peers_compact_data = peers_bytes.as_slice();

            for (id, _) in (0..peers_compact_data.len()).step_by(6).enumerate() {
                let ip = Ipv4Addr::from(peers_compact_data.get_u32());
                let port = peers_compact_data.get_u16();
                let peer = Peer::new(id, SocketAddrV4::new(ip, port), self.torrent_info.hashes.len());
    
                result.push(peer);
            }
        }

        result
    }

    pub async fn announce_in_tracker(&self) -> Vec<Peer> {
        let mut peers = Vec::new();

        Engine::check_torrent(self.torrent_info.clone()).await;

        println!("Announcing on tracker...");

        let missing_data = {
            let mut result = 0;

            for piece in self.torrent_info.pieces.read().await.iter() {
                if !piece.finished() {
                    result += piece.piece_len()
                }
            }

            result
        };

        let (_info, hash) = self.torrent_info.data.info();

        let announce_url = format!(
            "{}?info_hash={}&peer_id={}&port=6881&left={}&downloaded=0&uploaded=0&compact=1",
            self.torrent_info.data.announce(),
            urlencoding::encode_binary(&hash),
            self.client_id,
            missing_data
        );

        for attempt in 0..5 {
            let response = reqwest::get(&announce_url).await;

            if let Ok(tracker_response) = response {
                let response_body = tracker_response.bytes().await.unwrap().to_vec();
                
                println!("Announced successfully");
                peers = self.update_peer_list(response_body);
                
                break;
            }
            else if let Err(error) = response {
                if let Some(code) = error.status() {
                    println!(
                        "Failed to announce on tracker {} (code {}). Retrying... ({}/5)",
                        self.torrent_info.data.announce(),
                        code,
                        attempt + 1
                    );
                }
                else {
                    println!(
                        "Failed to announce on tracker {}. Retrying... ({}/5)",
                        self.torrent_info.data.announce(),
                        attempt + 1
                    );
                }
                
            }
        }

        peers
    }

    pub async fn start_torrent(self) {
        let peers = self.announce_in_tracker().await;

        let mut handles = Vec::new();
        let missing_pieces = Arc::new(RwLock::new(Engine::get_missing_pieces(self.torrent_info.clone()).await));

        let (have_tx, _) = tokio::sync::broadcast::channel(10);

        for peer in peers {
            let info = self.torrent_info.clone();
            let missing_pieces_p = missing_pieces.clone();

            let have_tx_peer = have_tx.clone();
            let have_rx_peer = have_tx.subscribe();

            let handle = tokio::task::spawn(async {
                let mut peer = peer;

                let have_tx = have_tx_peer;
                let mut have_rx = have_rx_peer;

                let info = info;
                let rng = fastrand::Rng::new();
                let missing_pieces = missing_pieces_p;

                peer.connect_to_peer(&info.handshake_data).await;
                peer.send_message(MessageKind::Bitfield {available_pieces: info.bitfield.read().await.clone()}).await;

                loop {
                    if peer.requested_piece().is_none() {
                        let lock = missing_pieces.read().await;
                        let piece = rng.usize(..lock.len());
    
                        if peer.has_piece(piece) {
                            peer.set_requested_piece(piece);
                        }
                    }
    
                    if let Some(message) = peer.handle_messages().await {
                        let mut pieces = info.pieces.write().await;

                        match message {
                            MessageKind::Request { piece_index, block_offset, block_length } => {
                                let (start_file, start_position) = pieces[piece_index].get_offsets();
                                let piece = Engine::read_piece(info.clone(), start_file, start_position).await;
        
                                let block_data = &piece[block_offset..block_offset+block_length];
                                let block_data = block_data.to_vec();
                                
                                let message = MessageKind::Piece {
                                    piece_index,
                                    block_offset,
                                    block_data
                                };
        
                                peer.send_message(message).await;
                            }
                            MessageKind::Piece { piece_index, block_offset, block_data } => {
                                if piece_index >= pieces.len() {
                                    continue;
                                }
            
                                for byte in block_data.iter() {
                                    peer.piece_data_mut().push(*byte);
                                }
    
                                peer.set_waiting_for_response(false);
                                pieces[piece_index].set_requested_to_peer(false);
                                pieces[piece_index].add_received_bytes(block_data.len());
    
                                if Engine::check_piece(info.clone(), piece_index, peer.piece_data()) {
                                    let data = peer.piece_finished();
                                    let (mut file_idx, mut file_position) = pieces[piece_index].get_offsets();
    
                                    Engine::write_piece(info.clone(), &data, &mut file_idx, &mut file_position).await;
    
                                    println!("Finished piece {}.", piece_index);

                                    have_tx.send(piece_index).unwrap();
                                    pieces[piece_index].set_finished(true);
    
                                    if !missing_pieces.read().await.is_empty() {
                                        let pieces_len = missing_pieces.read().await.len();
                                        let piece_idx = rng.usize(..pieces_len);
                    
                                        if peer.has_piece(piece_idx) {
                                            peer.set_requested_piece(missing_pieces.write().await.remove(piece_idx));
                                        }
                                    }
                                }
                            }
                            MessageKind::Cancel { .. } => {}
                            _ => {}
                        }
                    }

                    if let Ok(piece_index) = have_rx.try_recv() {
                        if !peer.peer_choking_client() {
                            peer.send_message(MessageKind::Have { piece_index }).await;
                        }
                    }
    
                    if !peer.waiting_for_response() && !peer.peer_choking_client() {
                        if peer.client_choking_peer() {
                            peer.send_message(MessageKind::Unchoke).await;
                        }
    
                        if !peer.client_interested() {
                            peer.send_message(MessageKind::Interested).await;
                        }
    
                        if let Some(piece_idx) = peer.requested_piece() {
                            let piece_index = *piece_idx;
    
                            if let Some(piece) = info.pieces.write().await.get_mut(piece_index) {
                                if !piece.finished() && !piece.requested_to_peer() {
                                    let (block_offset, block_length) = piece.get_block_request();
                                    let peer_message = MessageKind::Request {
                                        piece_index,
                                        block_offset,
                                        block_length
                                    };
    
                                    peer.send_message(peer_message).await;
                                    piece.set_requested_to_peer(true);
                                }
                            }
                        }
                    }

                    task::yield_now().await;
                }


            });

            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }
    }

    fn get_handshake(id: &str, info_hash: &[u8; 20]) -> Vec<u8> {
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

    async fn build_file_list(files_data: Vec<(String, i64)>, piece_len: usize) -> Vec<File> {
        let mut list = Vec::new();

        for (filename, size) in files_data {
            list.push(File::new(filename, size as usize, piece_len).await);
        }

        list
    }

    async fn build_hashes_list(data: Vec<u8>) -> Vec<[u8; 20]> {
        let mut data = data.as_slice();
        let mut list = Vec::new();

        while !data.is_empty() {
            let mut hash = [0; 20];
            
            if data.read_exact(&mut hash).await.is_ok() {
                list.push(hash);
            }
        }

        list
    }

    async fn check_torrent(info: Arc<TorrentInfo>) {
        let check_time = std::time::Instant::now();

        let total_hashes = info.hashes.len();
        let mut new_hashes: Vec<[u8; 20]> = Vec::with_capacity(total_hashes);

        let mut current_file = 0;
        let mut current_piece = 0;
        let mut current_file_offset = 0;

        println!("Checking loaded torrent...");

        while current_piece != total_hashes {
            let start_file = current_file;
            let start_position = current_file_offset as usize;
            let filesize = info.files.write().await[current_file].file_mut().metadata().await.unwrap().len() as usize;
            
            let piece = Engine::read_piece(info.clone(), current_file, current_file_offset).await;

            let end_position = current_file_offset as usize + piece.len();

            match end_position.cmp(&filesize) {
                std::cmp::Ordering::Greater => {
                    let missing_data = end_position - filesize;

                    current_file += 1;
                    current_file_offset = missing_data;
                }
                std::cmp::Ordering::Equal => {
                    current_file += 1;
                    current_file_offset = 0;    
                }
                std::cmp::Ordering::Less => current_file_offset += piece.len()
            }

            let hash = Sha1::from(&piece).digest().bytes();
            let piece = Piece::new(piece.len(), start_file, start_position, hash == info.hashes[current_piece]);
            
            current_piece += 1;

            new_hashes.push(hash);
            info.pieces.write().await.push(piece);
        }

        let mut good_pieces: i32 = 0;
        let mut hash_idx = 0;

        'bitfield: loop {
            let mut byte = 0;

            for bit in (0..8).rev() {
                if let Some(good_hash) = info.hashes.get(hash_idx) {
                    let hash = &new_hashes[hash_idx];

                    if hash == good_hash {
                        byte |= 1 << bit;
                        good_pieces += 1;
                    }

                    hash_idx += 1;
                }
                else {
                    break 'bitfield;
                }
            }

            info.bitfield.write().await.push(byte);
        }
        
        println!("Checked torrent in {}s, got {} complete pieces.", check_time.elapsed().as_secs(), good_pieces);
    }

    fn check_piece(info: Arc<TorrentInfo>, idx: usize, data: &[u8]) -> bool {
        let hash = Sha1::from(data).digest().bytes();

        hash == info.hashes[idx]
    }

    async fn read_piece(info: Arc<TorrentInfo>, start_file: usize, start_position: usize) -> Vec<u8> {
        let piece_length = info.piece_length;
        let last_file = start_file == info.files.read().await.len() - 1;
        let mut piece = info.files.write().await[start_file].read_piece(start_position as u64).await;
                        
        if !last_file && piece.len() < piece_length {
            let remaining_data = piece_length - piece.len();
            let mut data = info.files.write().await[start_file + 1].read_piece(0).await;
        
            data.resize(remaining_data, 0);
            piece.append(&mut data);
        }

        piece
    }

    async fn write_piece(info: Arc<TorrentInfo>, data: &[u8], file_idx: &mut usize, file_position: &mut usize) {
        let file_count = info.files.read().await.len();
        let mut files_lock = info.files.write().await;
        let mut file = files_lock[*file_idx].file_mut();

        if file_count == 1 {
            file.seek(SeekFrom::Start(*file_position as u64)).await.unwrap();
            file.write_all(data).await.unwrap();
        }
        else {
            for byte in data {
                let filesize = file.metadata().await.unwrap().len();
    
                if *file_position as u64 >= filesize {
                    *file_idx += 1;
                    *file_position = 0;
                        
                    file = files_lock[*file_idx].file_mut();
                }
                    
                *file_position += 1;
                file.write_all(&[*byte]).await.unwrap();
            }
        }
    }

    async fn get_missing_pieces(info: Arc<TorrentInfo>) -> Vec<usize> {
        let mut result = Vec::with_capacity(info.hashes.len());
        let mut piece_idx = 0;

        for byte in info.bitfield.read().await.iter() {
            for bit in (0..8).rev() {
                if (*byte >> bit) & 1 == 0 {
                    result.push(piece_idx);
                }

                piece_idx += 1;
            }
        }

        result
    }
}
