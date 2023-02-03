use tokio::fs;
use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

pub struct File {
    file: fs::File,
    piece_length: u64
}

impl File {
    pub async fn new(filename: String, size: u64, piece_length: u64) -> File {
        let mut path = dirs::download_dir().unwrap();
        path.push(filename);

        tokio::fs::create_dir_all(path.parent().unwrap()).await.expect("Failed to create directory for file.");

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .await
            .unwrap()
        ;

        if file.metadata().await.unwrap().len() < size {
            file.seek(SeekFrom::Start(size - 1)).await.unwrap();
            file.write_all(&[0]).await.unwrap();
        }

        File {
            file,
            piece_length
        }
    }

    pub async fn read_piece(&mut self, offset: u64) -> Vec<u8> {
        let filesize = self.file.metadata().await.unwrap().len();
        self.file.seek(SeekFrom::Start(offset)).await.unwrap();

        let mut buffer = {
            if offset + self.piece_length > filesize {
                vec![0; (filesize - offset) as usize]
            }
            else {
                vec![0; self.piece_length as usize]
            }
        };

        self.file.read_exact(&mut buffer).await.unwrap();
        buffer
    }

    pub async fn get_file_size(&self) -> u64 {
        if let Ok(metadata) = self.file.metadata().await {
            metadata.len()
        }
        else {
            0
        }
    }

    pub fn file_mut(&mut self) -> &mut fs::File {
        &mut self.file
    }
}
