use super::types::{Message, PeerCommunicator};
use async_trait::async_trait;
use bitvec::prelude::*;
use snafu::{ensure, Snafu};
use std::io;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufStream},
    net::{TcpStream, ToSocketAddrs},
};

pub struct TcpPeerCommunicator {
    stream: BufStream<TcpStream>,
}

impl TcpPeerCommunicator {
    pub async fn new<A: ToSocketAddrs>(
        addr: A,
        info_hash: &[u8],
        peer_id: &[u8],
    ) -> Result<Self, TcpPeerError> {
        let mut stream = BufStream::new(TcpStream::connect(addr).await?);

        log::debug!(
            "Opened TcpStream for address: {}",
            stream.get_ref().peer_addr().unwrap()
        );

        // Send handshake
        stream
            .write_all(
                &[
                    // 0x13 is 19 in decimal, which is the pstrlen. The next eight bytes
                    // after the pstr itself are 0s, which are obviously 0x00. These are
                    // the reserved bytes.
                    b"\x13BitTorrent protocol\x00\x00\x00\x00\x00\x00\x00\x00",
                    info_hash,
                    peer_id,
                ]
                .concat(),
            )
            .await?;

        stream.flush().await?;

        log::debug!("Sent handshake to peer");

        let mut buf = vec![0; 68];

        // Recieve handshake
        stream.read_exact(&mut buf).await?;

        log::debug!(
            "Recieved handshake reply from peer: {:?}",
            String::from_utf8_lossy(&buf)
        );

        ensure!(&buf[28..48] == info_hash, HandshakeInfoHash);

        Ok(TcpPeerCommunicator { stream })
    }
}

#[async_trait]
impl PeerCommunicator for TcpPeerCommunicator {
    type Err = TcpPeerError;

    async fn read(&mut self) -> Result<Option<Message>, Self::Err> {
        use Message::*;

        let len = self.stream.read_u32().await?;

        if len == 0 {
            return Ok(None);
        }

        let id = self.stream.read_u8().await?;

        log::trace!("len bytes = {:?}", len.to_be_bytes());
        log::debug!("Message with len = {}, id = {}", len, id);

        Ok(Some(match id {
            0 => Choke,
            1 => Unchoke,
            2 => Interested,
            3 => NotInterested,
            4 => {
                ensure!(len == 5, InvalidMessageLen { len, id });

                Have {
                    piece_index: self.stream.read_u32().await?,
                }
            }
            5 => {
                let mut bitfield = vec![0; (len - 1) as usize];

                self.stream.read_exact(&mut bitfield).await?;

                BitField {
                    bitfield: BitVec::from_vec(bitfield),
                }
            }
            6 | 8 => {
                ensure!(len == 13, InvalidMessageLen { len, id });

                let index = self.stream.read_u32().await?;
                let begin = self.stream.read_u32().await?;
                let length = self.stream.read_u32().await?;

                if id == 6 {
                    Request {
                        index,
                        begin,
                        length,
                    }
                } else {
                    Cancel {
                        index,
                        begin,
                        length,
                    }
                }
            }
            7 => {
                ensure!(len >= 9, InvalidMessageLen { len, id });

                let index = self.stream.read_u32().await?;
                let begin = self.stream.read_u32().await?;

                // TODO: reuse buffers across read calls?
                // maybe store it in the struct itself?
                let mut block = vec![0; (len - 9) as usize];

                self.stream.read_exact(&mut block).await?;

                Piece {
                    index,
                    begin,
                    block,
                }
            }
            id => InvalidMessage { id }.fail()?,
        }))
    }

    async fn write(&mut self, message: Message) -> Result<(), Self::Err> {
        use Message::*;

        let (id, payload) = match message {
            Choke => (0, vec![]),
            Unchoke => (1, vec![]),
            Interested => (2, vec![]),
            NotInterested => (3, vec![]),
            Have { piece_index } => (4, piece_index.to_be_bytes().to_vec()),
            BitField { bitfield } => (5, bitfield.into_vec()),
            Request {
                index,
                begin,
                length,
            }
            | Cancel {
                index,
                begin,
                length,
            } => (
                if let Request { .. } = message { 6 } else { 8 },
                [
                    index.to_be_bytes(),
                    begin.to_be_bytes(),
                    length.to_be_bytes(),
                ]
                .concat(),
            ),
            Piece {
                index,
                begin,
                block,
            } => (
                7,
                [&index.to_be_bytes() as &[u8], &begin.to_be_bytes(), &block].concat(),
            ),
        };

        self.stream.write_u32(payload.len() as u32 + 1).await?;
        self.stream.write_u8(id).await?;
        self.stream.write_all(&payload).await?;

        self.stream.flush().await?;

        Ok(())
    }
}

#[derive(Debug, Snafu)]
pub enum TcpPeerError {
    #[snafu(context(false))]
    StreamError { source: io::Error },
    #[snafu(display("Recieved a message with an unknown ID: {}", id))]
    InvalidMessage { id: u8 },
    #[snafu(display("Recieved a message with an invalid length {} for its ID {}", len, id))]
    InvalidMessageLen { len: u32, id: u8 },
    #[snafu(display("Recieved a handshake with a different info hash than was sent"))]
    HandshakeInfoHash,
}