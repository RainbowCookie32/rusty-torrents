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
        // First 4 bytes are a u32 representing the length of the message.
        let mut len_buf = vec![0; 4];
        let mut message = None;

        let read_len_bytes = self.stream.try_read(&mut len_buf).ok()?;

        if read_len_bytes == 4 {
            let msg_length = len_buf.as_slice().get_u32() as usize;
            let mut msg_buf = vec![0; msg_length];

            let read_msg_bytes = self.stream.read_exact(&mut msg_buf).await.ok()?;

            if read_msg_bytes == msg_length {
                message = Some(msg_buf.into());
            }
        }

        message
    }

    async fn send_message(&mut self, message: Message) -> bool {
        let buf: Vec<u8> = message.into();
        self.stream.write_all(&buf).await.is_ok()
    }

    async fn handshake_peer(&mut self, buf: Vec<u8>) -> bool {
        if self.stream.write_all(&buf).await.is_ok() {
            let mut response = vec![0; 68];
            let received_bytes = self.stream.read(&mut response).await.unwrap_or(0);

            received_bytes == response.len()
        }
        else {
            false
        }
    }
}
