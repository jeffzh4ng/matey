#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_tracker_url};

use async_trait::async_trait;
use bencode_parser::{parse_bencode, Bencode};
use snafu::{ensure, Snafu};
use std::{
    convert::TryInto,
    io,
    net::{IpAddr, SocketAddr},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufStream},
    net::{TcpStream, ToSocketAddrs},
};
use bitvec::prelude::*;

const PORT: u16 = 6881;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let peer_id = build_peer_id();

    log::info!("Our peer ID: {}", peer_id);

    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;

    let torrent = Torrent::try_from(torrent_bytes)?;

    log::info!("Parsed torrent file");

    let tracker_url = build_tracker_url(&torrent, &PORT.to_string(), &peer_id)?;

    log::info!("Announcing to tracker at URL: {}", tracker_url);

    let resp = reqwest::get(tracker_url).await?;

    log::info!("Got response from tracker");

    let peerlist = build_peerlist(&resp.bytes().await?)
        .ok_or("Failed to find valid peerlist in tracker response")?;

    log::info!("Parsed peerlist from tracker response");

    // This attempts a connection with each peer in turn until one succeeds.
    let mut peer = TcpPeerCommunicator::new(
        peerlist.as_slice(),
        torrent.info_hash.as_ref(),
        peer_id.as_bytes(),
    )
    .await?;

    log::info!("Established BitTorrent connection with peer");

    let mut choked = true;
    let mut request = false;

    loop {
        match peer.read().await {
            Ok(Some(msg)) => {
                if let Message::Piece {
                    index,
                    begin,
                    block,
                } = &msg
                {
                    log::info!("Recieved piece {} at offset {}:\n {:?}", index, begin, String::from_utf8_lossy(block));
                } else {
                    log::info!("Recieved message from peer: {:?}", &msg);
                }

                if choked {
                    // TODO: Provide a way to pipeline writes? Builder pattern, maybe?
                    peer.write(Message::Unchoke).await?;
                    peer.write(Message::Interested).await?;

                    choked = false;

                    log::info!("Sent unchoke and interested to peer");
                }

                if msg == Message::Unchoke && !request {
                    log::info!("Peer unchoked us, sending request");

                    peer.write(Message::Request {
                        index: 0,
                        begin: 0,
                        length: 2u32.pow(10), // 2**10 = 1024 bytes or 1KB
                    })
                    .await?;

                    request = true;
                }
            }
            Ok(None) => {
                log::debug!("Peer sent keep-alive message");
            }
            Err(e) => {
                log::error!("failed to read token from socket; err = {:?}", e);
                break;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Message {
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
trait PeerCommunicator {
    type Err;

    async fn read(&mut self) -> Result<Option<Message>, Self::Err>;
    async fn write(&mut self, message: Message) -> Result<(), Self::Err>;
}

struct TcpPeerCommunicator {
    stream: BufStream<TcpStream>,
}

impl TcpPeerCommunicator {
    async fn new<A: ToSocketAddrs>(
        addr: A,
        info_hash: &[u8],
        peer_id: &[u8],
    ) -> Result<Self, TcpPeerError> {
        let mut stream = BufStream::new(TcpStream::connect(addr).await?);

        log::debug!("Opened TcpStream for address: {}", stream.get_ref().peer_addr().unwrap());

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

        log::debug!("Recieved handshake reply from peer: {:?}", String::from_utf8_lossy(&buf));

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
            BitField { bitfield } => (
                5,
                bitfield.into_vec()
            ),
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
enum TcpPeerError {
    #[snafu(context(false))]
    StreamError { source: io::Error },
    #[snafu(display("Recieved a message with an unknown ID: {}", id))]
    InvalidMessage { id: u8 },
    #[snafu(display("Recieved a message with an invalid length {} for its ID {}", len, id))]
    InvalidMessageLen { len: u32, id: u8 },
    #[snafu(display("Recieved a handshake with a different info hash than was sent"))]
    HandshakeInfoHash,
}

fn build_peerlist(response: &[u8]) -> Option<Vec<SocketAddr>> {
    let (_, response_bencode) = parse_bencode(response).ok()?;

    let mut response_dict = response_bencode.dict()?;

    let (peers, v6_peers) = (
        response_dict.remove(b"peers" as &[u8]),
        response_dict.remove(b"peers6" as &[u8]),
    );

    if peers.is_none() && v6_peers.is_none() {
        return None;
    }

    match (
        peers.unwrap_or_else(|| Bencode::ByteString(vec![])),
        v6_peers.unwrap_or_else(|| Bencode::ByteString(vec![])),
    ) {
        // compact mode
        (Bencode::ByteString(v4_peer_bytes), Bencode::ByteString(v6_peer_bytes)) => {
            let (v4_chunks, v4_remainder) = v4_peer_bytes.as_chunks::<6>();
            let (v6_chunks, v6_remainder) = v6_peer_bytes.as_chunks::<18>();

            if !(v4_remainder.is_empty() && v6_remainder.is_empty()) {
                return None;
            }

            Some(
                v4_chunks
                    .iter()
                    .map(|&addr_bytes| {
                        (
                            IpAddr::from(<[u8; 4]>::try_from(&addr_bytes[0..4]).unwrap()),
                            u16::from_be_bytes(addr_bytes[4..].try_into().unwrap()),
                        )
                    })
                    .chain(v6_chunks.iter().map(|&addr_bytes| {
                        (
                            IpAddr::from(<[u8; 16]>::try_from(&addr_bytes[0..16]).unwrap()),
                            u16::from_be_bytes(addr_bytes[16..].try_into().unwrap()),
                        )
                    }))
                    .map(SocketAddr::from)
                    .collect(),
            )
        }
        // non-compact mode
        (Bencode::List(peer_list), _) => peer_list
            .into_iter()
            .map(|peer| {
                let mut peer_dict = peer.dict()?;

                let ip = peer_dict
                    .remove(b"ip" as &[u8])
                    .and_then(|val| val.byte_string())
                    .and_then(|bytes| String::from_utf8_lossy(&bytes).parse().ok())?;

                let port = peer_dict
                    .remove(b"port" as &[u8])
                    .and_then(|val| val.number())
                    .and_then(|num| u16::try_from(num).ok())?;

                Some(SocketAddr::new(ip, port))
            })
            .collect(),
        _ => None,
    }
}
