use std::net::SocketAddrV4;

use bytes::Buf;
use async_trait::async_trait;

use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::engine::peer::message::Message;
use crate::engine::peer::connection::PeerConnection;

pub struct TcpConnection {
    stream: TcpStream
}

impl TcpConnection {
    pub async fn connect(address: &SocketAddrV4) -> Option<TcpConnection> {
        if let Ok(stream) = TcpStream::connect(&address).await {
            Some(
                TcpConnection {
                    stream
                }
            )
        }
        else {
            None
        }
    }
}

#[async_trait]
impl PeerConnection for TcpConnection {
    async fn get_message(&mut self) -> Option<Message> {
        // Try to read the message's length.
        let mut buf = vec![0; 4];
        let mut message = None;

        if let Ok(bytes) = self.stream.try_read(&mut buf) {
            if bytes == 4 {
                let length = buf.as_slice().get_u32() as usize;
                let mut buf = vec![0; length];

                if let Ok(bytes) = self.stream.read_exact(&mut buf).await {
                    if bytes == length {
                        message = Message::from_bytes(buf);
                    }
                }
            }
        }

        message
    }

    async fn send_message(&mut self, message: Message) -> bool {
        let buf = message.to_bytes();
        self.stream.write_all(&buf).await.is_ok()
    }

    async fn handshake_peer(&mut self, buf: Vec<u8>) -> bool {
        if self.stream.write_all(&buf).await.is_ok() {
            let mut response = vec![0; 68];

            if let Ok(bytes) = self.stream.read(&mut response).await {
                bytes == response.len()   
            }
            else {
                false
            }
        }
        else {
            false
        }
    }
}
