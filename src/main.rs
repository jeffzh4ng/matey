#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{collections::HashSet, convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_tracker_url};

use async_trait::async_trait;
use bencode_parser::{parse_bencode, Bencode};
use std::{
    convert::TryInto,
    io,
    net::{IpAddr, SocketAddr},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufStream},
    net::{TcpStream, ToSocketAddrs},
};

const PORT: u16 = 6881;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let peer_id = build_peer_id();

    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;

    let torrent = Torrent::try_from(torrent_bytes)?;

    let tracker_url = build_tracker_url(&torrent, &PORT.to_string(), &peer_id)?;

    let resp = reqwest::get(tracker_url).await?;

    println!("----Announced to tracker----");

    let peerlist = build_peerlist(&resp.bytes().await?)
        .ok_or("Failed to find valid peerlist in tracker response")?;

    println!("----Creating TcpPeerCommunicator----");
    
    // This attempts a connection with each peer in turn until one succeeds.
    let mut peer = TcpPeerCommunicator::new(peerlist.as_slice(), torrent.info_hash.as_ref(), peer_id.as_bytes()).await?;

    let mut choked = true;
    let mut request = false;

    loop {
        match peer.read().await {
            Ok(Some(msg)) => {
                dbg!(&msg);

                if choked {
                    // TODO: Provide a way to pipeline writes? Builder pattern, maybe?
                    peer.write(Message::Unchoke).await?;
                    peer.write(Message::Interested).await?;

                    choked = false;

                    println!("----Sent unchoked & interested----");
                }

                if msg == Message::Unchoke && !request {
                    println!("----Peer unchoked us, sending request----");

                    peer.write(Message::Request {
                        index: 0,
                        begin: 0,
                        length: 2u32.pow(10), // 2**10 = 1024 bytes or 1KB
                    }).await?;

                    request = true;
                }
            },
            Ok(None) => {
                println!("Peer sent keep-alive message");
            },
            Err(e) => {
                eprintln!("----failed to read token from socket; err = {:?}----", e);
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
        bitfield: HashSet<u32>,
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
    async fn new<A: ToSocketAddrs>(addr: A, info_hash: &[u8], peer_id: &[u8]) -> io::Result<Self> {
        // 0x13 is 19 in decimal, which is the pstrlen. The next eight bytes
        // after the pstr itself are 0s, which are obviously 0x00. These are
        // the reserved bytes.
        let header = b"\x13BitTorrent protocol\x00\x00\x00\x00\x00\x00\x00\x00";

        let mut stream = BufStream::new(TcpStream::connect(addr).await?);

        println!("----Sending handshake to {}----", stream.get_ref().peer_addr()?);

        // Send handshake
        stream.write_all(
            &[
                header,
                info_hash,
                peer_id,
            ]
            .concat(),
        )
        .await?;

        println!("----Sent handshake----");

        let mut buf = vec![0; 68];

        loop {
            let n = stream.read(&mut buf).await?;

            if n == 0 {
                panic!("Connection closed");
            }

            dbg!(&buf, String::from_utf8_lossy(&buf));

            if n == 68 {
                break;
            }
        }

        // // Recieve handshake
        // // TODO: custom error handling
        // stream.read_exact(&mut buf).await?;

        println!("----Recieved handshake----");

        if &buf[0..28] != header {
            panic!("Handshake does not match");
        }

        if &buf[28..48] != info_hash {
            panic!("Infohash does not match");
        }

        Ok(TcpPeerCommunicator { stream })
    }
}

#[async_trait]
impl PeerCommunicator for TcpPeerCommunicator {
    type Err = io::Error;

    async fn read(&mut self) -> Result<Option<Message>, Self::Err> {
        use Message::*;

        let len = self.stream.read_u32().await?;

        if len == 0 {
            return Ok(None);
        }

        let id = self.stream.read_u8().await?;

        Ok(Some(if len == 1 {
            match id {
                0 => Choke,
                1 => Unchoke,
                2 => Interested,
                3 => NotInterested,
                _ => todo!(), // TODO: return custom error type
            }
        } else {
            // TODO: reuse buffers across read calls?
            // maybe store it in the struct itself?
            let mut payload = vec![0; len as usize - 1];
            self.stream.read_exact(&mut payload).await?;

            match id {
                4 => Have {
                    piece_index: u32::from_be_bytes(payload.try_into().unwrap()),
                },
                5 => BitField { bitfield: HashSet::new() }, // TODO: parse the bitfield,
                6 => {
                    // TODO: ensure len == 13

                    Request {
                        index: u32::from_be_bytes(payload[0..4].try_into().unwrap()),
                        begin: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
                        length: u32::from_be_bytes(payload[8..12].try_into().unwrap()),
                    }
                },
                7 => {
                    // TODO: ensure len >= 9

                    let block = payload.split_off(8);

                    Piece {
                        index: u32::from_be_bytes(payload[0..4].try_into().unwrap()),
                        begin: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
                        block,
                    }
                },
                8 => {
                    // TODO: ensure len == 13

                    Cancel {
                        index: u32::from_be_bytes(payload[0..4].try_into().unwrap()),
                        begin: u32::from_be_bytes(payload[4..8].try_into().unwrap()),
                        length: u32::from_be_bytes(payload[8..12].try_into().unwrap()),
                    }
                },
                _ => todo!() // TODO: return custom error type
            }
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
            BitField { bitfield } => todo!(),
            Request {
                index,
                begin,
                length,
            } => (
                6,
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
            Cancel {
                index,
                begin,
                length,
            } => (
                8,
                [
                    index.to_be_bytes(),
                    begin.to_be_bytes(),
                    length.to_be_bytes(),
                ]
                .concat(),
            ),
        };

        self.stream.write_u32(payload.len() as u32 + 1).await?;
        self.stream.write_u8(id).await?;
        self.stream.write_all(&payload).await?;

        self.stream.flush().await?;

        Ok(())
    }
}

// TODO: deal with peers & peers6 sometimes either being there,
// or both being there
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
