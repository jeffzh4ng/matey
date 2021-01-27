use async_trait::async_trait;
use bitvec::prelude::*;
use std::{error, fmt};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockMeta {
    pub piece_index: u32,
    pub begin: u32,
    pub length: u32,
}

impl fmt::Debug for BlockMeta {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("BlockMeta")
            .field(&self.piece_index)
            .field(&self.begin)
            .field(&self.length)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block {
    pub meta: BlockMeta,
    pub data: Vec<u8>,
}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Block")
            .field("meta", &self.meta)
            .field("data", &"..")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    BitField(BitVec<Msb0, u8>),
    Request(BlockMeta),
    Piece(Block),
    Cancel(BlockMeta),
}

#[async_trait]
pub trait PeerReader {
    type Error: error::Error;

    async fn read(&mut self) -> Result<Message, Self::Error>;
}

#[async_trait]
pub trait PeerWriter {
    type Error: error::Error;

    async fn write(&mut self, message: Message) -> Result<(), Self::Error>;
    async fn flush(&mut self) -> Result<(), Self::Error>;
}
