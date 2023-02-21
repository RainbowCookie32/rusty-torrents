use std::sync::Arc;
use std::path::Path;
use std::io::SeekFrom;

use sha1::Sha1;

use tokio::fs;
use tokio::fs::File;
use tokio::fs::OpenOptions;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use crate::bencode::ParsedTorrent;

pub struct Transfer {
    /// SHA-1 hasher.
    sha1: Sha1,

    /// Hash of the info section of the torrent file.
    info_hash: Arc<[u8; 20]>,
    /// The parsed .torrent file.
    torrent_data: ParsedTorrent,
    
    /// Length of each piece. Final piece might be smaller.
    piece_length: u64,
    /// The size of each individual piece.
    pieces_sizes: Vec<usize>,
    /// Whether a piece is complete or not.
    pieces_status: Vec<bool>,
    /// The file and position where each piece starts.
    pieces_offsets: Vec<(usize, usize)>,

    /// Handles for each file of the torrent.
    files: Vec<File>,
    
    left: u64,
    total_size: u64,
    total_uploaded: u64,
    total_downloaded: u64,
}

impl Transfer {
    pub async fn create(torrent_data: ParsedTorrent, output_path: &Path) -> Transfer {
        let sha1 = Sha1::default();
        let info_hash = Arc::new(*torrent_data.info().info_hash());
        
        let piece_count = torrent_data.info().pieces().len();
        let piece_length = torrent_data.info().piece_length();

        let mut files = Vec::with_capacity(torrent_data.get_files().len());

        for (filename, size) in torrent_data.get_files() {
            let mut file_path = output_path.to_path_buf();
            file_path.push(filename);

            files.push(Transfer::create_file(&file_path, *size).await);
        }

        let pieces_offsets = Transfer::calculate_pieces_offsets(&files, piece_count, piece_length).await;

        let total_size = torrent_data.get_files()
            .iter()
            .map(| (_, length) | *length)
            .sum()
        ;

        Transfer {
            sha1,

            info_hash,
            torrent_data,

            piece_length,
            pieces_sizes: Vec::with_capacity(piece_count),
            pieces_status: vec![false; piece_count],
            pieces_offsets,
            
            files,

            left: total_size,
            total_size,
            total_uploaded: 0,
            total_downloaded: 0,
        }
    }

    pub fn info_hash(&self) -> Arc<[u8; 20]> {
        self.info_hash.clone()
    }

    pub fn piece_length(&self, piece_idx: usize) -> u64 {
        self.pieces_sizes[piece_idx] as u64
    }

    pub fn pieces_status(&self) -> &Vec<bool> {
        &self.pieces_status
    }

    pub fn is_complete(&self) -> bool {
        self.pieces_status
            .iter()
            .filter(| piece | !(**piece))
            .count() == 0
    }

    pub fn get_progress(&self) -> TransferProgress {
        TransferProgress {
            left: self.left,
            total: self.total_size,
            uploaded: self.total_uploaded,
            downloaded: self.total_downloaded,
        }
    }

    pub fn get_trackers(&self) -> Vec<String> {
        let mut trackers_list = Vec::with_capacity(self.torrent_data.announce_list().len() + 1);
        trackers_list.push(self.torrent_data.announce().to_owned());

        self.torrent_data.announce_list()
            .iter()
            .filter(| url | !url.is_empty())
            .for_each(| url | trackers_list.push(url.to_owned()))
        ;

        trackers_list.sort_unstable();
        trackers_list.dedup();

        trackers_list
    }

    pub async fn check_torrent(&mut self) {
        let piece_count = self.pieces_status.len();
        let mut new_status = vec![false; piece_count];

        self.left = self.total_size;

        for piece in 0..piece_count {
            new_status[piece] = self.check_piece(piece).await;
        }

        self.pieces_status = new_status;
    }

    async fn check_piece(&mut self, piece: usize) -> bool {
        assert!(piece < self.pieces_status.len());

        let piece_data = self.read_piece(piece).await;
        let piece_hash = &self.torrent_data.info().pieces()[piece];

        self.sha1.reset();
        self.sha1.update(&piece_data);

        self.pieces_sizes.push(piece_data.len());

        if self.sha1.digest().bytes() == piece_hash.as_slice() {
            self.left -= piece_data.len() as u64;
            true
        }
        else {
            false
        }
    }

    pub async fn add_complete_piece(&mut self, piece: usize, data: Vec<u8>) -> bool {
        assert!(piece < self.pieces_status.len());

        let piece_hash = &self.torrent_data.info().pieces()[piece];

        self.sha1.reset();
        self.sha1.update(&data);

        if self.sha1.digest().bytes() == piece_hash.as_slice() {
            self.left -= data.len() as u64;
            self.pieces_status[piece] = true;
            
            self.write_piece(piece, data).await;
            true
        }
        else {
            false
        }
    }

    async fn read_piece(&mut self, piece: usize) -> Vec<u8> {
        assert!(piece < self.pieces_status.len());

        let piece_length = self.piece_length as usize;
        let (piece_file_idx, piece_offset) = self.pieces_offsets[piece];
        let mut piece_file = &mut self.files[piece_file_idx];

        let file_size = piece_file.metadata().await.unwrap().len();
        let buf_size = {
            let remaining = file_size as usize - piece_offset;

            if remaining >= piece_length {
                piece_length
            }
            else {
                remaining
            }
        };

        let mut piece_buf = vec![0; buf_size];

        piece_file
            .seek(SeekFrom::Start(piece_offset as u64))
            .await
            .expect("file seek failed")
        ;

        if let Ok(read_bytes) = piece_file.read_exact(&mut piece_buf).await {
            if read_bytes != piece_length && piece != (self.pieces_status.len() - 1) {
                let missing_bytes = piece_length - read_bytes;
                let mut remaining_buf = vec![0; missing_bytes];

                piece_buf.resize(read_bytes, 0);
                piece_file = &mut self.files[piece_file_idx + 1];

                piece_file
                    .seek(SeekFrom::Start(0))
                    .await
                    .expect("file seek failed")
                ;

                if let Ok(read_bytes) = piece_file.read_exact(&mut remaining_buf).await {
                    if read_bytes == remaining_buf.len() {
                        piece_buf.append(&mut remaining_buf);
                    }
                    else {
                        panic!("failed to read the complete piece");
                    }
                }
            }
        }

        piece_buf
    }

    async fn write_piece(&mut self, piece: usize, data: Vec<u8>) {
        assert!(piece < self.pieces_status.len());

        let (piece_file_idx, piece_offset) = &self.pieces_offsets[piece];
        let mut piece_file = &mut self.files[*piece_file_idx];

        let file_size = piece_file.metadata().await.unwrap().len();
        let split_piece = *piece_offset + data.len() > file_size as usize;

        piece_file
            .seek(SeekFrom::Start(*piece_offset as u64))
            .await
            .expect("file seek failed")
        ;

        if split_piece {
            let bytes_to_write = file_size as usize - *piece_offset;
            let first_batch = &data[0..bytes_to_write];
            let second_batch = &data[bytes_to_write..];

            piece_file.write_all(first_batch).await.expect("failed to write piece");

            piece_file = &mut self.files[*piece_file_idx + 1];
            piece_file.write_all(second_batch).await.expect("failed to write piece");
        }
        else {
            piece_file.write_all(data.as_slice()).await.expect("failed to write piece");
        }
    }

    async fn create_file(path: &Path, size: u64) -> File {
        fs::create_dir_all(path.parent().unwrap()).await
            .expect("Failed to create download dir")
        ;

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await
            .expect("Failed to open/create file")
        ;

        let file_size = file
            .metadata()
            .await
            .expect("Failed to get file metadata")
            .len()
        ;

        if file_size < size {
            file.seek(SeekFrom::Start(size - 1)).await
                .expect("Failed to seek to end of file");
            file.write_all(&[0]).await
                .expect("Failed to extend file to size");
        }

        file
    }

    async fn calculate_pieces_offsets(files: &[File], piece_count: usize, piece_length: u64) -> Vec<(usize, usize)> {
        let mut pieces_offsets = Vec::with_capacity(piece_count);

        let mut file_idx = 0;
        let mut file_position = 0;

        let mut file_metadata = files[0].metadata().await.unwrap();

        for i in 0..piece_count {
            let is_last_piece = i == piece_count - 1;
            pieces_offsets.push((file_idx, file_position));

            file_position += piece_length as usize;

            if file_position >= file_metadata.len() as usize && !is_last_piece {
                file_idx += 1;
                file_position -= file_metadata.len() as usize;
    
                file_metadata = files[file_idx].metadata().await.unwrap();
            }
        }

        pieces_offsets
    }
}

#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub left: u64,
    pub total: u64,
    pub uploaded: u64,
    pub downloaded: u64
}

impl TransferProgress {
    pub fn new(left: u64, total: u64, uploaded: u64, downloaded: u64) -> TransferProgress {
        TransferProgress {
            left,
            total,
            uploaded,
            downloaded
        }
    }
}
