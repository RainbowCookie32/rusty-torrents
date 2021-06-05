use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum BEncodeType {
    BInt { value: i64 },
    BString { value: String, bytes: Vec<u8> },

    BList { entries: Vec<BEncodeType> },
    BDictionary { entries: HashMap<String, BEncodeType>, hash: [u8; 20] }
}

#[derive(Debug)]
pub enum ParseError {
    BadLength(usize),
    MissingSection(String),
    UnknownFileFormat
}

impl BEncodeType {
    pub fn string(data: &[u8], position: &mut usize) -> Result<BEncodeType, ParseError> {
        let result = BEncodeType::parse_string(data, position);
        
        if let Ok((value, bytes)) = result {
            Ok(BEncodeType::BString { value, bytes })
        }
        else {
            Err(result.unwrap_err())
        }
    }

    fn parse_string(data: &[u8], position: &mut usize) -> Result<(String, Vec<u8>), ParseError> {
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

            string_data.parse()
        };

        if let Ok(string_length) = string_length {
            let mut result = String::with_capacity(string_length);
            let mut result_bytes = Vec::new();

            for offset in 0..string_length {
                result.push(data[*position + offset] as char);
                result_bytes.push(data[*position + offset]);
            }

            *position += string_length;

            Ok((result, result_bytes))
        }
        else {
            Err(ParseError::BadLength(*position))
        }
    }

    pub fn integer(data: &[u8], position: &mut usize) -> Result<BEncodeType, ParseError> {
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

        if let Ok(value) = result.parse() {
            Ok(BEncodeType::BInt { value })
        }
        else {
            Err(ParseError::BadLength(*position))
        }
    }

    pub fn list(data: &[u8], position: &mut usize) -> Result<BEncodeType, ParseError> {
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

            if let Ok(value) = value {
                entries.push(value);
            }
            else if let Err(error) = value {
                return Err(error);
            }

            if data[*position] as char == 'e' {
                *position += 1;
                break;
            }
        }

        Ok(BEncodeType::BList { entries })
    }

    pub fn dictionary(data: &[u8], position: &mut usize) -> Result<BEncodeType, ParseError> {
        let start = *position as u16;
        let mut entries = HashMap::new();

        loop {
            let key = BEncodeType::parse_string(data, position);

            if let Ok((key, _)) = key {
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

                if let Ok(value) = value {
                    entries.insert(key, value);
                }
                else if let Err(error) = value {
                    return Err(error);
                }
            }
            else if let Err(error) = key {
                return Err(error);
            }

            if data[*position] as char == 'e' {
                *position += 1;
                break;
            }
        }

        let dictionary_bytes = &data[start as usize - 1..*position];
        let hash = sha1::Sha1::from(dictionary_bytes).digest().bytes();

        Ok(BEncodeType::BDictionary { entries, hash })
    }

    pub fn get_int(&self) -> i64 {
        if let BEncodeType::BInt { value } = self {
            *value
        }
        else {
            -1
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
    announce: BEncodeType,
    announce_list: BEncodeType,
    creation_date: BEncodeType,
    
    info: BEncodeType
}

impl ParsedTorrent {
    pub fn new(data: Vec<u8>) -> Result<ParsedTorrent, ParseError> {
        let mut position = 0;
        let value_byte = data[position] as char;
    
        position += 1;
    
        if value_byte == 'd' {
            let result = BEncodeType::dictionary(&data, &mut position);

            if let Ok(torrent_data) = result {
                let entries = torrent_data.get_dictionary();

                let announce = {
                    if let Some(entry) = entries.get("announce") {
                        entry.clone()
                    }
                    else {
                        return Err(ParseError::MissingSection(String::from("announce")))
                    }
                };
            
                let announce_list = {
                    if let Some(entry) = entries.get("announce-list") {
                        entry.clone()
                    }
                    else {
                        return Err(ParseError::MissingSection(String::from("announce-list")))
                    }
                };
            
                let creation_date = {
                    if let Some(entry) = entries.get("creation date") {
                        entry.clone()
                    }
                    else {
                        return Err(ParseError::MissingSection(String::from("creation date")))
                    }
                };
            
                let info = {
                    if let Some(entry) = entries.get("info") {
                        entry.clone()
                    }
                    else {
                        return Err(ParseError::MissingSection(String::from("info")))
                    }
                };
            
                Ok(ParsedTorrent {
                    announce,
                    announce_list,
                    creation_date,
            
                    info
                })
            }
            else if let Err(error) = result {
                Err(error)
            }
            else {
                unreachable!()
            }
        }
        else {
            Err(ParseError::UnknownFileFormat)
        }
    }

    pub fn announce(&self) -> String {
        self.announce.get_string()
    }

    pub fn announce_list(&self) -> &BEncodeType {
        &self.announce_list
    }

    pub fn creation_date(&self) -> &BEncodeType {
        &self.creation_date
    }

    pub fn info(&self) -> (HashMap<String, BEncodeType>, [u8; 20]) {
        (self.info.get_dictionary(), self.info.get_dictionary_hash())
    }

    pub fn get_name(&self) -> String {
        if let Some(name) = self.info.get_dictionary().get("name") {
            return name.get_string();
        }
        
        panic!("Couldn't get torrent's name");
    }

    pub fn get_files(&self) -> Vec<(String, i64)> {
        let info = self.info.get_dictionary();
        let torrent_name = self.get_name();

        if let Some(files) = info.get("files") {
            let file_list = files.get_list();
            let mut files = Vec::new();

            for entry in file_list {
                let file_info = entry.get_dictionary();
                
                if let (Some(path), Some(length)) = (file_info.get("path"), file_info.get("length")) {
                    let filename = path.get_list();
                    let full_path = format!("{}/{}", torrent_name, filename[0].get_string());

                    files.push((full_path, length.get_int()));
                }
            }

            files
        }
        else if let Some(length) = info.get("length") {
            vec![(torrent_name, length.get_int())]
        }
        else {
            panic!("couldn't find torrent's name");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn torrent_parsing() {
        let file_data = std::fs::read("/home/rainbowcookie/Downloads/ubuntu-21.04-desktop-amd64.iso.torrent").unwrap();
        let result = crate::ParsedTorrent::new(file_data);
        
        assert!(result.is_ok())
    }
}
