pub mod tcp;

use std::net::SocketAddrV4;

use async_trait::async_trait;

use crate::engine::peer::Message;

#[async_trait]
pub trait PeerConnection {
    async fn get_message(&mut self) -> Option<Message>;
    async fn send_message(&mut self, message: Message) -> bool;
    async fn handshake_peer(&mut self, buf: Vec<u8>) -> bool;
}

pub async fn create_connection(address: &SocketAddrV4) -> Option<Box<dyn PeerConnection + Send>> {
    if let Some(connection) = tcp::TcpConnection::connect(address).await {
        Some(Box::new(connection))
    }
    else {
        None
    }
}
