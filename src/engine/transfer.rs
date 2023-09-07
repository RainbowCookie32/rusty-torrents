use std::sync::Arc;
use std::path::Path;
use std::io::SeekFrom;
use std::net::SocketAddr;

use sha1_smol::Sha1;

use tokio::fs;
use tokio::fs::File;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::bencode::ParsedTorrent;

pub struct Transfer {
    /// SHA-1 hasher.
    sha1: Sha1,

    /// Hash of the info section of the torrent file.
    info_hash: Arc<[u8; 20]>,
    /// The parsed .torrent file.
    torrent_data: ParsedTorrent,

    /// Info about all the pieces on this torrent.
    pieces: Vec<TransferPiece>,
    /// Length of each piece. Final piece might be smaller.
    piece_length: u64,

    /// Handles for each file of the torrent.
    files: Vec<(File, u64)>,
    
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

        let pieces = Transfer::calculate_pieces_info(&files, piece_count, piece_length).await;

        let total_size = torrent_data.get_files()
            .iter()
            .map(| (_, length) | *length)
            .sum()
        ;

        Transfer {
            sha1,

            info_hash,
            torrent_data,

            pieces,
            piece_length,
            
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

    pub fn piece_count(&self) -> usize {
        self.pieces.len()
    }

    pub fn pieces_status(&self) -> Vec<bool> {
        self.pieces
            .iter()
            .map(| piece | piece.completed)
            .collect()
    }

    pub fn get_piece_for_peer(&mut self, available_pieces: &Vec<bool>) -> Option<&mut TransferPiece> {
        if available_pieces.is_empty() {
            return None;
        }
        
        self.pieces.iter_mut()
            .filter(| piece | !piece.completed && piece.assigned_to.is_none())
            .find(| piece | available_pieces[piece.idx])
    }

    pub fn unassign_piece(&mut self, piece: usize) {
        self.pieces[piece].assigned_to = None;
    }

    pub fn is_complete(&self) -> bool {
        self.pieces
            .iter()
            .filter(| piece | !piece.completed)
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
        let piece_count = self.pieces.len();
        self.left = self.total_size;

        for piece in 0..piece_count {
            self.pieces[piece].completed = self.check_piece(piece).await;
        }
    }

    async fn check_piece(&mut self, piece: usize) -> bool {
        assert!(piece < self.pieces.len());

        let piece_data = self.read_piece(piece).await;
        let piece_hash = &self.torrent_data.info().pieces()[piece];

        self.sha1.reset();
        self.sha1.update(&piece_data);

        self.pieces[piece].length = piece_data.len();

        if self.sha1.digest().bytes() == piece_hash.as_slice() {
            self.left -= piece_data.len() as u64;
            true
        }
        else {
            false
        }
    }

    pub async fn add_complete_piece(&mut self, piece: usize, data: Vec<u8>) -> bool {
        assert!(piece < self.pieces.len());

        let piece_hash = &self.torrent_data.info().pieces()[piece];

        self.sha1.reset();
        self.sha1.update(&data);

        if self.sha1.digest().bytes() == piece_hash.as_slice() {
            self.left -= data.len() as u64;
            self.pieces[piece].completed = true;
            
            self.write_piece(piece, data).await;
            true
        }
        else {
            false
        }
    }

    pub async fn read_piece(&mut self, piece: usize) -> Vec<u8> {
        assert!(piece < self.pieces.len());

        let piece_length = self.piece_length as usize;
        let piece_file_i = self.pieces[piece].start_file;
        let piece_offset = self.pieces[piece].start_position;

        let (piece_file, file_size) = &mut self.files[piece_file_i];

        let buf_size = {
            let remaining = *file_size as usize - piece_offset;

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
            if read_bytes != piece_length && piece != (self.pieces.len() - 1) {
                let missing_bytes = piece_length - read_bytes;
                let mut remaining_buf = vec![0; missing_bytes];

                piece_buf.resize(read_bytes, 0);
                let (piece_file, _) = &mut self.files[piece_file_i as usize + 1];

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
        assert!(piece < self.pieces.len());

        let piece_length = data.len();
        let piece_file_i = self.pieces[piece].start_file as usize;
        let piece_offset = self.pieces[piece].start_position as usize;
        
        let (piece_file, file_size) = &mut self.files[piece_file_i];

        let split_piece = piece_offset as usize + piece_length > *file_size as usize;

        piece_file
            .seek(SeekFrom::Start(piece_offset as u64))
            .await
            .expect("file seek failed")
        ;

        // FIXME: This code assumes a piece can only contain data for 2 files.
        // This assumption was made with 0 evidence to back it up.
        if split_piece {
            let bytes_to_write = *file_size as usize - piece_offset;
            let first_batch = &data[0..bytes_to_write];
            let second_batch = &data[bytes_to_write..];

            piece_file.write_all(first_batch).await.expect("failed to write piece");

            let (piece_file, _) = &mut self.files[piece_file_i + 1];
            
            piece_file
                .seek(SeekFrom::Start(0))
                .await
                .expect("file seek failed")
            ;

            piece_file.write_all(second_batch).await.expect("failed to write piece");
        }
        else {
            piece_file.write_all(data.as_slice()).await.expect("failed to write piece");
        }
    }

    async fn create_file(path: &Path, size: u64) -> (File, u64) {
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

        (file, size)
    }

    async fn calculate_pieces_info(files: &[(File, u64)], piece_count: usize, piece_length: u64) -> Vec<TransferPiece> {
        let mut pieces_info = Vec::with_capacity(piece_count);

        let mut file_idx = 0;
        let mut file_position = 0;

        let (_, mut file_size) = files[0];

        for i in 0..piece_count {
            let start_file = file_idx;
            let start_position = file_position;

            let piece_info = TransferPiece {
                idx: i,
                length: piece_length as usize,
                completed: false,

                start_file,
                start_position,

                assigned_to: None
            };

            pieces_info.push(piece_info);

            let is_last_piece = i == piece_count - 1;

            file_position += piece_length as usize;

            if file_position >= file_size as usize && !is_last_piece {
                file_idx += 1;
                file_position -= file_size as usize;

                let (_, next_file_size) = files[file_idx];
    
                file_size = next_file_size;
            }
        }

        pieces_info
    }
}

pub struct TransferPiece {
    idx: usize,
    length: usize,
    completed: bool,

    start_file: usize,
    start_position: usize,

    assigned_to: Option<SocketAddr>,
}

impl TransferPiece {
    pub fn idx(&self) -> usize {
        self.idx
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn set_assigned(&mut self, address: SocketAddr) {
        self.assigned_to = Some(address);
    }
}

#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub left: u64,
    pub total: u64,
    pub uploaded: u64,
    pub downloaded: u64
}

