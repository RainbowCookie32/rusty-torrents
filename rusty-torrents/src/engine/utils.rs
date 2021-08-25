use std::sync::Arc;

use sha1::Sha1;

use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::engine::TorrentInfo;
use crate::engine::piece::Piece;

pub async fn check_piece(info: Arc<TorrentInfo>, idx: usize, data: &[u8]) -> bool {
    let hash = Sha1::from(data).digest().bytes();

    if hash == info.pieces_hashes[idx].as_slice() {
        info.bitfield_client.write().await.piece_finished(idx as u32);
        true
    }
    else {
        false
    }
}

pub async fn read_piece(info: Arc<TorrentInfo>, start_file: usize, start_position: usize) -> Vec<u8> {
    let piece_length = info.piece_length;
    let last_file = start_file == info.torrent_files.read().await.len() - 1;
    let mut piece = info.torrent_files.write().await[start_file].read_piece(start_position as u64).await;
                    
    if !last_file && piece.len() < piece_length {
        let remaining_data = piece_length - piece.len();
        let mut data = info.torrent_files.write().await[start_file + 1].read_piece(0).await;
    
        data.resize(remaining_data, 0);
        piece.append(&mut data);
    }

    piece
}

pub async fn write_piece(info: Arc<TorrentInfo>, data: &[u8], file_idx: &mut usize, file_position: &mut usize) {
    let file_count = info.torrent_files.read().await.len();
    let mut files_lock = info.torrent_files.write().await;
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

pub async fn check_torrent(info: &mut Arc<TorrentInfo>) {
    let check_time = std::time::Instant::now();

    let total_hashes = info.pieces_hashes.len();
    let mut new_hashes: Vec<[u8; 20]> = Vec::with_capacity(total_hashes);

    let mut current_file = 0;
    let mut current_piece = 0;
    let mut current_file_offset = 0;

    println!("Checking loaded torrent...");

    while current_piece != total_hashes {
        let start_file = current_file;
        let start_position = current_file_offset as usize;
        let filesize = info.torrent_files.write().await[current_file].file_mut().metadata().await.unwrap().len() as usize;
        
        let piece = read_piece(info.clone(), current_file, current_file_offset).await;

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
        let finished = hash == info.pieces_hashes[current_piece].as_slice();
        let piece = Piece::new(piece.len(), start_file, start_position, finished);

        if finished {
            info.bitfield_client.write().await.piece_finished(current_piece as u32);
        }
        
        current_piece += 1;

        new_hashes.push(hash);
        info.torrent_pieces.write().await.push(piece);
    }
    
    println!("Checked {} pieces in {}s.", current_piece, check_time.elapsed().as_secs());
}

pub async fn update_missing_pieces(info: &mut Arc<TorrentInfo>) {
    let mut result = Vec::with_capacity(info.pieces_hashes.len());
    let mut piece_idx = 0;

    for byte in info.bitfield_client.read().await.as_bytes().iter() {
        for bit in (0..8).rev() {
            if (*byte >> bit) & 1 == 0 {
                result.push(piece_idx);
            }

            piece_idx += 1;
        }
    }

    *info.pieces_missing.write().await = result;
}
