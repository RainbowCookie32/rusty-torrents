use std::sync::Arc;

use tokio::sync::RwLock;

use crate::engine::file::File;
use crate::engine::TorrentInfo;
use crate::engine::piece::Piece;
use crate::engine::peer::Bitfield;

pub type TInfo = Arc<TorrentInfo>;
pub type THashes = Arc<Vec<Vec<u8>>>;
pub type TMissing = Arc<RwLock<Vec<usize>>>;
pub type TFiles = Arc<RwLock<Vec<File>>>;
pub type TPieces = Arc<RwLock<Vec<Piece>>>;
pub type TClientBitfield = Arc<RwLock<Bitfield>>;
