pub struct Piece {
    piece_len: usize,

    start_file: usize,
    start_position: usize,

    finished: bool,
    requested_to_peer: bool,
    last_received_byte: usize
}

impl Piece {
    pub fn new(piece_len: usize, start_file: usize, start_position: usize, finished: bool) -> Piece {
        Piece {
            piece_len,
            start_file,
            start_position,

            finished,
            requested_to_peer: false,
            last_received_byte: 0
        }
    }

    pub fn get_offsets(&self) -> (usize, usize) {
        (self.start_file, self.start_position)
    }

    pub fn get_block_request(&self) -> (usize, usize) {
        let potential_size = self.last_received_byte + 16384;

        if potential_size > self.piece_len {
            (self.last_received_byte, self.piece_len - self.last_received_byte)
        }
        else {
            (self.last_received_byte, 16384)
        }
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn set_finished(&mut self, finished: bool) {
        self.finished = finished;
    }

    pub fn requested_to_peer(&self) -> bool {
        self.requested_to_peer
    }

    pub fn set_requested_to_peer(&mut self, requested_to_peer: bool) {
        self.requested_to_peer = requested_to_peer;
    }

    pub fn add_received_bytes(&mut self, len: usize) {
        self.last_received_byte += len;
    }

    pub fn piece_len(&self) -> usize {
        self.piece_len
    }
}
