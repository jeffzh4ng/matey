use async_trait::async_trait;
use bitvec::prelude::*;
use std::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockMeta {
    pub piece_index: u32,
    pub begin: u32,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block {
    pub meta: BlockMeta,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have { piece_index: u32 },
    BitField { bitfield: BitVec<Msb0, u8> },
    Request(BlockMeta),
    Piece(Block),
    Cancel(BlockMeta),
}

#[async_trait]
pub trait PeerCommunicator {
    type Err: error::Error;

    async fn read(&mut self) -> Result<Option<Message>, Self::Err>;
    async fn write(&mut self, message: Message) -> Result<(), Self::Err>;
}
