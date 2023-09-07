use std::io::{Cursor, Read};
use std::collections::HashMap;

const D_CHAR: u8 = 100;
const E_CHAR: u8 = 101;
const I_CHAR: u8 = 105;
const L_CHAR: u8 = 108;
const SEMICOLON_CHAR: u8 = 58;

#[derive(Clone, Debug)]
pub enum BEncodeType {
    Int { value: i64 },
    List { value: Vec<BEncodeType> },
    String { value: String, value_bytes: Vec<u8> },
    Dictionary { value: HashMap<String, BEncodeType>, value_hash: [u8; 20], value_hash_str: String }
}

impl BEncodeType {
    pub fn parse_type(cursor: &mut Cursor<&[u8]>) -> BEncodeType {
        let value_byte = {
            let mut buf = vec![0];
            cursor.read_exact(&mut buf).expect("failed to read from cursor");

            buf[0]
        };

        match value_byte {
            D_CHAR => BEncodeType::dictionary(cursor),
            L_CHAR => BEncodeType::list(cursor),
            I_CHAR => BEncodeType::integer(cursor),
            _ => {
                cursor.set_position(cursor.position() - 1);
                BEncodeType::string(cursor)
            }
        }
    }

    fn string(cursor: &mut Cursor<&[u8]>) -> BEncodeType {
        let (value, value_bytes) = {
            let mut sb_buf = vec![0];

            let length_str = {
                let mut length_buf = Vec::with_capacity(2);
    
                loop {
                    let value = {
                        cursor.read_exact(&mut sb_buf).expect("failed to read from cursor");
                        sb_buf[0]
                    };
    
                    if value != SEMICOLON_CHAR {
                        length_buf.push(value);
                    }
                    else {
                        break;
                    }
                }
                
                String::from_utf8(length_buf)
                    .expect("Failed to parse bencoded int as string")
                    .parse()
                    .expect("Bad string length.")
            };
    
            let mut result_bytes = vec![0; length_str];
            cursor.read_exact(&mut result_bytes).expect("failed to read from cursor");
    
            (String::from_utf8(result_bytes.clone()).unwrap_or_default(), result_bytes)
        };

        BEncodeType::String { value, value_bytes }
    }

    fn integer(cursor: &mut Cursor<&[u8]>) -> BEncodeType {
        let mut sb_buf = vec![0];
        let mut result_buf = Vec::with_capacity(2);

        loop {
            let byte = {
                cursor.read_exact(&mut sb_buf).expect("failed to read from cursor");
                sb_buf[0]
            };

            if byte != E_CHAR {
                result_buf.push(byte);
            }
            else {
                break;
            }
        }

        let result_str = String::from_utf8(result_buf)
            .expect("Failed to parse bencoded int as string")
        ;

        // TODO: Fail gracefully.
        BEncodeType::Int { value: result_str.parse().expect("failed to parse int value") }
    }

    fn list(cursor: &mut Cursor<&[u8]>) -> BEncodeType {
        let mut sb_buf = vec![0];
        let mut value = Vec::new();

        loop {
            let byte = {
                cursor.read_exact(&mut sb_buf).expect("failed to read from cursor");
                sb_buf[0]
            };

            if byte != E_CHAR {
                cursor.set_position(cursor.position() - 1);
                value.push(BEncodeType::parse_type(cursor));
            }
            else {
                break;
            }
        }

        BEncodeType::List { value }
    }

    fn dictionary(cursor: &mut Cursor<&[u8]>) -> BEncodeType {
        let mut sb_buf = vec![0];
        let mut bytes = Vec::new();
        let mut entries = HashMap::new();

        let end_position: usize;
        let start_position = cursor.position() as usize - 1;

        loop {
            let byte = {
                cursor.read_exact(&mut sb_buf).expect("failed to read from cursor");
                sb_buf[0]
            };

            if byte != E_CHAR {
                bytes.push(byte);
                cursor.set_position(cursor.position() - 1);
                
                let key = BEncodeType::string(cursor).as_string();
                entries.insert(key, BEncodeType::parse_type(cursor));
            }
            else {
                end_position = cursor.position() as usize;
                break;
            }
        }

        let dictionary_hash = sha1_smol::Sha1::from(&cursor.get_ref()[start_position..end_position]);
        let dictionary_hash_bytes = dictionary_hash.digest().bytes();

        BEncodeType::Dictionary { value: entries, value_hash: dictionary_hash_bytes, value_hash_str: dictionary_hash.hexdigest() }
    }

    pub fn as_dictionary(&self) -> HashMap<String, BEncodeType> {
        if let BEncodeType::Dictionary { value, .. } = self {
            value.clone()
        }
        else {
            println!("as_dictionary called on a BEncodeType value that wasn't a dictionary!");
            HashMap::new()
        }
    }

    pub fn as_dictionary_hash(&self) -> [u8; 20] {
        if let BEncodeType::Dictionary { value_hash, .. } = self {
            value_hash.clone()
        }
        else {
            panic!("as_dictionary_hash called on a BEncodeType value that wasn't a dictionary!");
        }
    }

    pub fn as_dictionary_hash_str(&self) -> String {
        if let BEncodeType::Dictionary { value_hash_str, .. } = self {
            value_hash_str.clone()
        }
        else {
            println!("as_dictionary_hash_str called on a BEncodeType value that wasn't a dictionary!");
            String::new()
        }
    }

    pub fn as_string(&self) -> String {
        if let BEncodeType::String { value, ..} = self {
            value.clone()
        }
        else {
            println!("as_string called on a BEncodeType value that wasn't a string! {:#?}", self);
            String::new()
        }
    }

    pub fn as_string_bytes(&self) -> Vec<u8> {
        if let BEncodeType::String { value_bytes, ..} = self {
            value_bytes.clone()
        }
        else {
            println!("as_string_bytes called on a BEncodeType value that wasn't a string!");
            Vec::new()
        }
    }

    pub fn as_list(&self) -> Vec<BEncodeType> {
        if let BEncodeType::List { value } = self {
            value.clone()
        }
        else {
            println!("as_list called on a BEncodeType value that wasn't a list!");
            Vec::new()
        }
    }

    pub fn as_integer(&self) -> i64 {
        if let BEncodeType::Int { value } = self {
            *value
        }
        else {
            println!("as_integer called on a BEncodeType value that wasn't an integer!");
            0
        }
    }
}

pub struct ParsedTorrent {
    announce: String,
    announce_list: Vec<String>,
    
    info: TorrentInfo
}

impl ParsedTorrent {
    pub fn new(data: Vec<u8>) -> ParsedTorrent {
        let mut data = Cursor::new(data.as_ref());
        let entries = BEncodeType::parse_type(&mut data).as_dictionary();

        let announce = {
            if let Some(entry) = entries.get("announce") {
                entry.as_string()
            }
            else {
                String::new()
            }
        };
        
        let announce_list = {
            if let Some(entry) = entries.get("announce-list") {
                let list = entry.as_list();
                list
                    .iter()
                    .map(| tracker_list | {
                        tracker_list.as_list()[0].clone()
                    })
                    .map(| tracker | tracker.as_string())
                    .collect()
            }
            else {
                Vec::new()
            }
        };
        
        let info = {
            if let Some(entry) = entries.get("info") {
                TorrentInfo::new(entry)
            }
            else {
                panic!("Couldn't find info section")
            }
        };
        
        ParsedTorrent {
            announce,
            announce_list,
    
            info
        }
    }

    pub fn announce(&self) -> &String {
        &self.announce
    }

    pub fn announce_list(&self) -> &Vec<String> {
        &self.announce_list
    }

    pub fn info(&self) -> &TorrentInfo {
        &self.info
    }

    pub fn get_name(&self) -> String {
        self.info.name.clone()
    }

    pub fn get_files(&self) -> &Vec<(String, u64)> {
        &self.info.files
    }
}

pub struct TorrentInfo {
    info_hash: [u8; 20],
    info_hash_str: String,

    length: u64,
    name: String,
    piece_length: u64,

    files: Vec<(String, u64)>,
    pieces: Vec<Vec<u8>>
}

impl TorrentInfo {
    pub fn new(info: &BEncodeType) -> TorrentInfo {
        let info_hash = info.as_dictionary_hash();
        let info_hash_str = info.as_dictionary_hash_str();

        let info = info.as_dictionary();

        let length = {
            if let Some(length) = info.get("length") {
                length.as_integer() as u64
            }
            else {
                0
            }
        };
        let name = info.get("name").unwrap().as_string();
        let piece_length = info.get("piece length").unwrap().as_integer() as u64;

        let files = {
            if let Some(files) = info.get("files") {
                let file_list = files.as_list();
                let mut files = Vec::new();
    
                for entry in file_list {
                    let file_info = entry.as_dictionary();
                    
                    if let (Some(path), Some(length)) = (file_info.get("path"), file_info.get("length")) {
                        let filename = path.as_list();
                        let full_path = format!("{}/{}", &name, filename[0].as_string());
    
                        files.push((full_path, length.as_integer() as u64));
                    }
                }
    
                files
            }
            else {
                vec![(name.clone(), length)]
            }
        };

        let pieces = {
            let bytes = info.get("pieces").unwrap().as_string_bytes();
            let chunks = bytes.chunks(20);
            let mut hashes = Vec::with_capacity(chunks.len());

            for chunk in chunks {
                hashes.push(chunk.to_owned());
            }

            hashes
        };

        TorrentInfo {
            info_hash,
            info_hash_str,

            length,
            name,
            piece_length,

            files,
            pieces
        }
    }

    /// Get a reference to the torrent info's info hash.
    pub fn info_hash(&self) -> &[u8; 20] {
        &self.info_hash
    }

    pub fn info_hash_str(&self) -> &str {
        &self.info_hash_str
    }

    /// Get a reference to the torrent info's length.
    pub fn length(&self) -> u64 {
        self.length
    }

    /// Get a reference to the torrent info's name.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Get a reference to the torrent info's piece length.
    pub fn piece_length(&self) -> u64 {
        self.piece_length
    }

    /// Get a reference to the torrent info's files.
    pub fn files(&self) -> &[(String, u64)] {
        self.files.as_slice()
    }

    /// Get a reference to the torrent info's pieces.
    pub fn pieces(&self) -> &[Vec<u8>] {
        self.pieces.as_slice()
    }
}

mod tests {
    #[test]
    fn fedora_torrent() {
        let file = std::fs::read("./torrents/Fedora-Workstation-Live-x86_64-37.torrent").expect("Failed to load torrent file");
        let parsed = super::ParsedTorrent::new(file);

        assert_eq!(parsed.announce(), "http://torrent.fedoraproject.org:6969/announce");
        assert_eq!(parsed.info().files(), &[
            (String::from("Fedora-Workstation-Live-x86_64-37/Fedora-Workstation-37-1.7-x86_64-CHECKSUM"), 1062),
            (String::from("Fedora-Workstation-Live-x86_64-37/Fedora-Workstation-Live-x86_64-37-1.7.iso"), 2037372928),
        ]);
        assert_eq!(parsed.info().name(), "Fedora-Workstation-Live-x86_64-37");
        assert_eq!(parsed.info().piece_length(), 262144);

        assert_eq!(parsed.info().pieces().len(), 7772);
    }

    #[test]
    fn ubuntu_torrent() {
        let file = std::fs::read("./torrents/ubuntu-22.10-desktop-amd64.iso.torrent").expect("Failed to load torrent file");
        let parsed = super::ParsedTorrent::new(file);

        assert_eq!(parsed.announce(), "https://torrent.ubuntu.com/announce");
        assert_eq!(parsed.announce_list(), &vec![
            "https://torrent.ubuntu.com/announce",
            "https://ipv6.torrent.ubuntu.com/announce"
        ]);
        assert_eq!(parsed.info().files(), &[(String::from("ubuntu-22.10-desktop-amd64.iso"), 4071903232)]);
        assert_eq!(parsed.info().name(), "ubuntu-22.10-desktop-amd64.iso");
        assert_eq!(parsed.info().piece_length(), 262144);

        assert_eq!(parsed.info().pieces().len(), 15534);
    }
}
