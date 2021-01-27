use super::types::{Block, BlockMeta, Message, PeerReader, PeerWriter};
use async_trait::async_trait;
use bitvec::prelude::*;
use snafu::{ensure, Snafu};
use std::io;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::{tcp, TcpStream},
};

pub async fn create_tcp_peer_rw(
    tcp_stream: TcpStream,
    info_hash: &[u8],
    peer_id: &[u8],
) -> Result<(TcpPeerReader, TcpPeerWriter), TcpPeerError> {
    log::debug!(
        "Opened TcpPeerCommunicator for address: {}",
        tcp_stream.peer_addr().unwrap()
    );

    let (reader, writer) = tcp_stream.into_split();
    let (mut reader, mut writer) = (BufReader::new(reader), BufWriter::new(writer));

    // Send handshake
    writer
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

    writer.flush().await?;

    log::debug!("Sent handshake to peer");

    let mut buf = vec![0; 68];

    // Recieve handshake
    reader.read_exact(&mut buf).await?;

    log::debug!(
        "Recieved handshake reply from peer: {:?}",
        String::from_utf8_lossy(&buf)
    );

    ensure!(&buf[28..48] == info_hash, HandshakeInfoHash);

    Ok((TcpPeerReader { reader }, TcpPeerWriter { writer }))
}

pub struct TcpPeerReader {
    reader: BufReader<tcp::OwnedReadHalf>,
}

#[async_trait]
impl PeerReader for TcpPeerReader {
    type Error = TcpPeerError;

    async fn read(&mut self) -> Result<Message, Self::Error> {
        use Message::*;

        let len = self.reader.read_u32().await?;

        if len == 0 {
            return Ok(KeepAlive);
        }

        let id = self.reader.read_u8().await?;

        Ok(match id {
            0 => Choke,
            1 => Unchoke,
            2 => Interested,
            3 => NotInterested,
            4 => {
                ensure!(len == 5, InvalidMessageLen { len, id });

                Have(self.reader.read_u32().await?)
            }
            5 => {
                let mut bitfield = vec![0; (len - 1) as usize];

                self.reader.read_exact(&mut bitfield).await?;

                BitField(BitVec::from_vec(bitfield))
            }
            6 | 8 => {
                ensure!(len == 13, InvalidMessageLen { len, id });

                let piece_index = self.reader.read_u32().await?;
                let begin = self.reader.read_u32().await?;
                let length = self.reader.read_u32().await?;

                let block_meta = BlockMeta {
                    piece_index,
                    begin,
                    length,
                };

                if id == 6 {
                    Request(block_meta)
                } else {
                    Cancel(block_meta)
                }
            }
            7 => {
                ensure!(len >= 9, InvalidMessageLen { len, id });

                let piece_index = self.reader.read_u32().await?;
                let begin = self.reader.read_u32().await?;

                let mut data = vec![0; (len - 9) as usize];

                self.reader.read_exact(&mut data).await?;

                Piece(Block {
                    meta: BlockMeta {
                        piece_index,
                        begin,
                        length: data.len() as u32,
                    },
                    data,
                })
            }
            id => InvalidMessage { id }.fail()?,
        })
    }
}

pub struct TcpPeerWriter {
    writer: BufWriter<tcp::OwnedWriteHalf>,
}

#[async_trait]
impl PeerWriter for TcpPeerWriter {
    type Error = TcpPeerError;

    async fn write(&mut self, message: Message) -> Result<(), Self::Error> {
        use Message::*;

        let (id, payload) = match message {
            KeepAlive => {
                return self
                    .writer
                    .write_u32(0)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.into());
            }
            Choke => (0, vec![]),
            Unchoke => (1, vec![]),
            Interested => (2, vec![]),
            NotInterested => (3, vec![]),
            Have(piece_index) => (4, piece_index.to_be_bytes().to_vec()),
            BitField(bitfield) => (5, bitfield.into_vec()),
            Request(BlockMeta {
                piece_index,
                begin,
                length,
            })
            | Cancel(BlockMeta {
                piece_index,
                begin,
                length,
            }) => (
                if let Request { .. } = message { 6 } else { 8 },
                [
                    piece_index.to_be_bytes(),
                    begin.to_be_bytes(),
                    length.to_be_bytes(),
                ]
                .concat(),
            ),
            Piece(Block {
                meta: BlockMeta {
                    piece_index, begin, ..
                },
                data,
            }) => (
                7,
                [
                    &piece_index.to_be_bytes() as &[u8],
                    &begin.to_be_bytes(),
                    &data,
                ]
                .concat(),
            ),
        };

        self.writer.write_u32(payload.len() as u32 + 1u32).await?;
        self.writer.write_u8(id).await?;
        self.writer.write_all(&payload).await?;

        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.flush().await?;

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
