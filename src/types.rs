use async_trait::async_trait;
use bitvec::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have {
        piece_index: u32,
    },
    BitField {
        bitfield: BitVec<Msb0, u8>,
    },
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
    Cancel {
        index: u32,
        begin: u32,
        length: u32,
    },
}

#[async_trait]
pub trait PeerCommunicator {
    type Err;

    async fn read(&mut self) -> Result<Option<Message>, Self::Err>;
    async fn write(&mut self, message: Message) -> Result<(), Self::Err>;
}