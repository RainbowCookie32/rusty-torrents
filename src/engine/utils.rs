use std::sync::Arc;

use sha1::Sha1;

use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::engine::TorrentInfo;
use crate::engine::piece::Piece;

pub async fn read_piece(info: Arc<TorrentInfo>, start_file: usize, start_position: usize) -> Vec<u8> {
    let piece_length = info.piece_length as usize;
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

pub async fn write_piece(info: Arc<TorrentInfo>, data: &[u8], file_idx: &mut usize, file_position: &mut usize) {
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

pub async fn check_torrent(info: Arc<TorrentInfo>) {
    let hashes = info.data.info().pieces().to_vec();

    let mut current_file = 0;
    let mut current_piece = 0;
    let mut current_file_offset = 0;

    while current_piece < hashes.len() {
        let start_file = current_file;
        let start_offset = current_file_offset as usize;
        let piece_hash = hashes[current_piece].as_slice();
        let file_size = info.files.read().await[current_file].get_file_size().await as usize;
        
        let piece_data = read_piece(info.clone(), current_file, current_file_offset).await;
        let end_offset = current_file_offset as usize + piece_data.len();

        match end_offset.cmp(&file_size) {
            std::cmp::Ordering::Greater => {
                let missing_data = end_offset - file_size;

                current_file += 1;
                current_file_offset = missing_data;
            }
            std::cmp::Ordering::Equal => {
                current_file += 1;
                current_file_offset = 0;    
            }
            std::cmp::Ordering::Less => current_file_offset += piece_data.len()
        }

        let finished = Sha1::from(&piece_data).digest().bytes() == piece_hash;
        let piece = Piece::new(piece_data.len(), piece_hash.to_owned(), start_file, start_offset, finished);

        if finished {
            info.bitfield_client.write().await.piece_finished(current_piece as u32);
        }
        
        current_piece += 1;
        info.pieces.write().await.push(piece);
    }
}
