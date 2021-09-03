pub struct Piece {
    piece_len: usize,
    piece_data: Vec<u8>,
    piece_hash: Vec<u8>,

    start_file: usize,
    start_position: usize,

    finished: bool,
    requested: bool
}

impl Piece {
    pub fn new(piece_len: usize, piece_hash: Vec<u8>, start_file: usize, start_position: usize, finished: bool) -> Piece {
        let piece_data = Vec::with_capacity(piece_len);

        Piece {
            piece_len,
            piece_data,
            piece_hash,

            start_file,
            start_position,

            finished,
            requested: false
        }
    }

    pub fn get_offsets(&self) -> (usize, usize) {
        (self.start_file, self.start_position)
    }

    pub fn get_block_request(&self) -> (u32, u32) {
        let potential_size = self.piece_data.len() + 16384;

        if potential_size > self.piece_len {
            (self.piece_data.len() as u32, (self.piece_len - self.piece_data.len()) as u32)
        }
        else {
            (self.piece_data.len() as u32, 16384)
        }
    }

    pub fn check_piece(&self) -> bool {
        sha1::Sha1::from(&self.piece_data).digest().bytes() == self.piece_hash.as_slice()
    }

    pub fn reset_piece(&mut self) {
        self.finished = false;
        self.requested = false;
        self.piece_data = Vec::with_capacity(self.piece_len);
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn set_finished(&mut self, finished: bool) {
        self.finished = finished;
    }

    pub fn requested(&self) -> bool {
        self.requested
    }

    pub fn set_requested(&mut self, requested: bool) {
        self.requested = requested;
    }

    pub fn add_received_bytes(&mut self, buf: Vec<u8>) -> bool {
        let mut buf = buf;
        
        self.piece_data.append(&mut buf);
        self.piece_data.len() >= self.piece_len
    }

    pub fn piece_data(&self) -> &[u8] {
        self.piece_data.as_slice()
    }
}
