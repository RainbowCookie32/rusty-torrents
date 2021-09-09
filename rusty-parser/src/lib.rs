use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum BEncodeType {
    BInt { value: u64 },
    BString { value: String, bytes: Vec<u8> },

    BList { entries: Vec<BEncodeType> },
    BDictionary { entries: HashMap<String, BEncodeType>, hash: [u8; 20] }
}

impl BEncodeType {
    pub fn string(data: &[u8], position: &mut usize) -> BEncodeType {
        let (value, bytes) = BEncodeType::parse_string(data, position);

        BEncodeType::BString { value, bytes }
    }

    fn parse_string(data: &[u8], position: &mut usize) -> (String, Vec<u8>) {
        let string_length = {
            let mut string_data = String::new();

            loop {
                let value = data[*position] as char;
                
                *position += 1;

                if value != ':' {
                    string_data.push(value);
                }
                else {
                    break;
                }
            }

            string_data.parse().expect("Bad string length.")
        };

        let mut result = String::with_capacity(string_length);
        let mut result_bytes = Vec::new();

        for offset in 0..string_length {
            result.push(data[*position + offset] as char);
            result_bytes.push(data[*position + offset]);
        }

        *position += string_length;

        (result, result_bytes)
    }

    pub fn integer(data: &[u8], position: &mut usize) -> BEncodeType {
        let mut result = String::new();

        loop {
            let byte = data[*position] as char;

            *position += 1;

            if byte != 'e' {
                result.push(byte);
            }
            else {
                break;
            }
        }

        let value = result.parse().expect("Bad int length.");

        BEncodeType::BInt { value }
    }

    pub fn list(data: &[u8], position: &mut usize) -> BEncodeType {
        let mut entries = Vec::new();

        loop {
            let value_byte = data[*position] as char;

            *position += 1;

            let value = match value_byte {
                'd' => BEncodeType::dictionary(data, position),
                'l' => BEncodeType::list(data, position),
                'i' => BEncodeType::integer(data, position),
                _ => {

                    *position -= 1;
                    BEncodeType::string(data, position)
                }
            };

            entries.push(value);

            if data[*position] as char == 'e' {
                *position += 1;
                break;
            }
        }

        BEncodeType::BList { entries }
    }

    pub fn dictionary(data: &[u8], position: &mut usize) -> BEncodeType {
        let start = *position as u16;
        let mut entries = HashMap::new();

        loop {
            let (key, _) = BEncodeType::parse_string(data, position);

            let value_byte = data[*position] as char;

            *position += 1;

            let value = match value_byte {
                'd' => BEncodeType::dictionary(data, position),
                'l' => BEncodeType::list(data, position),
                'i' => BEncodeType::integer(data, position),
                _ => {
                    *position -= 1;
                    BEncodeType::string(data, position)
                }
            };

            entries.insert(key, value);

            if data[*position] as char == 'e' {
                *position += 1;
                break;
            }
        }

        let dictionary_bytes = &data[start as usize - 1..*position];
        let hash = sha1::Sha1::from(dictionary_bytes).digest().bytes();

        BEncodeType::BDictionary { entries, hash }
    }

    pub fn get_int(&self) -> u64 {
        if let BEncodeType::BInt { value } = self {
            *value
        }
        else {
            u64::MAX
        }
    }

    pub fn get_string(&self) -> String {
        if let BEncodeType::BString { value, ..} = self {
            value.clone()
        }
        else {
            String::new()
        }
    }

    pub fn get_string_bytes(&self) -> Vec<u8> {
        if let BEncodeType::BString { bytes, ..} = self {
            bytes.clone()
        }
        else {
            Vec::new()
        }
    }

    pub fn get_list(&self) -> Vec<BEncodeType> {
        if let BEncodeType::BList { entries } = self {
            entries.clone()
        }
        else {
            Vec::new()
        }
    }

    pub fn get_dictionary(&self) -> HashMap<String, BEncodeType> {
        if let BEncodeType::BDictionary { entries, .. } = self {
            entries.clone()
        }
        else {
            HashMap::new()
        }
    }

    pub fn get_dictionary_hash(&self) -> [u8; 20] {
        if let BEncodeType::BDictionary { hash, ..} = self {
            *hash
        }
        else {
            [0; 20]
        }
    }
}

pub struct ParsedTorrent {
    announce: String,
    announce_list: Vec<String>,
    creation_date: String,
    
    info: TorrentInfo
}

impl ParsedTorrent {
    pub fn new(data: Vec<u8>) -> ParsedTorrent {
        let mut position = 0;
        let value_byte = data[position] as char;
    
        position += 1;
    
        if value_byte == 'd' {
            let entries = BEncodeType::dictionary(&data, &mut position).get_dictionary();

            let announce = {
                if let Some(entry) = entries.get("announce") {
                    entry.get_string()
                }
                else {
                    String::new()
                }
            };
        
            let announce_list = {
                if let Some(entry) = entries.get("announce-list") {
                    entry.get_list().iter().map(|e| e.get_string()).collect()
                }
                else {
                    Vec::new()
                }
            };
        
            let creation_date = {
                if let Some(entry) = entries.get("creation date") {
                    entry.get_string()
                }
                else {
                    String::new()
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
                creation_date,
        
                info
            }
        }
        else {
            panic!("Malformed torrent file")
        }
    }

    pub fn announce(&self) -> &String {
        &self.announce
    }

    pub fn announce_list(&self) -> &Vec<String> {
        &self.announce_list
    }

    pub fn creation_date(&self) -> &String {
        &self.creation_date
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

    length: u64,
    name: String,
    piece_length: u64,

    files: Vec<(String, u64)>,
    pieces: Vec<Vec<u8>>
}

impl TorrentInfo {
    pub fn new(info: &BEncodeType) -> TorrentInfo {
        let info_hash = info.get_dictionary_hash();
        let info = info.get_dictionary();

        let length = {
            if let Some(length) = info.get("length") {
                length.get_int()
            }
            else {
                0
            }
        };
        let name = info.get("name").unwrap().get_string();
        let piece_length = info.get("piece length").unwrap().get_int();

        let files = {
            if let Some(files) = info.get("files") {
                let file_list = files.get_list();
                let mut files = Vec::new();
    
                for entry in file_list {
                    let file_info = entry.get_dictionary();
                    
                    if let (Some(path), Some(length)) = (file_info.get("path"), file_info.get("length")) {
                        let filename = path.get_list();
                        let full_path = format!("{}/{}", &name, filename[0].get_string());
    
                        files.push((full_path, length.get_int()));
                    }
                }
    
                files
            }
            else {
                vec![(name.clone(), length)]
            }
        };

        let pieces = {
            let bytes = info.get("pieces").unwrap().get_string_bytes();
            let chunks = bytes.chunks(20);
            let mut hashes = Vec::with_capacity(chunks.len());

            for chunk in chunks {
                hashes.push(chunk.to_owned());
            }

            hashes
        };

        TorrentInfo {
            info_hash,

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
