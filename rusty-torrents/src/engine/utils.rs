use std::sync::Arc;

use sha1::Sha1;

use tokio::io::{AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::engine::TorrentInfo;
use crate::engine::piece::Piece;

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

pub async fn check_torrent(info: Arc<TorrentInfo>) {
    let hashes = info.data.info().pieces().to_vec();
    let total_hashes = hashes.len();

    let mut current_file = 0;
    let mut current_piece = 0;
    let mut current_file_offset = 0;

    while current_piece != total_hashes {
        let start_file = current_file;
        let start_position = current_file_offset as usize;
        let filesize = info.torrent_files.write().await[current_file].file_mut().metadata().await.unwrap().len() as usize;
        
        let piece = read_piece(info.clone(), current_file, current_file_offset).await;
        let piece_hash = hashes[current_piece].as_slice();
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

        let finished = Sha1::from(&piece).digest().bytes() == piece_hash;
        let piece = Piece::new(piece.len(), piece_hash.to_owned(), start_file, start_position, finished);

        if finished {
            info.bitfield_client.write().await.piece_finished(current_piece as u32);
        }
        
        current_piece += 1;
        info.torrent_pieces.write().await.push(piece);
    }
}
