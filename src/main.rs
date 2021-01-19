#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_tracker_url};

use bencode_parser::{parse_bencode, Bencode};
use std::{
    convert::TryInto,
    net::{IpAddr, SocketAddr},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
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

    println!("----Attempting handshake----");

    // This attempts a connection with each peer in turn until one succeeds.
    let mut peer = TcpStream::connect(peerlist.as_slice()).await?;

    // Handshake
    peer.write_all(
        &[
            // 0x13 is 19 in decimal, which is the pstrlen. The next eight bytes
            // after the pstr itself are 0s, which are obviously 0x00. These are
            // the reserved bytes.
            b"\x13BitTorrent protocol\x00\x00\x00\x00\x00\x00\x00\x00" as &[u8],
            torrent.info_hash.as_ref(),
            peer_id.as_bytes(),
        ]
        .concat(),
    )
    .await?;

    println!("----Sent handshake to peer {}----", peer.peer_addr()?);

    let mut buf = [0u8; 1024];
    let mut choked = true;
    let mut request = false;

    loop {
        match peer.read(&mut buf).await {
            Ok(n) if n == 0 => {
                println!("----Connection closed----");
                break;
            }
            Ok(n) => {
                let msg = &buf[..n];
                println!("\n{:?}\n{}\n", msg, String::from_utf8_lossy(msg));

                if choked {
                    // unchoke (id = 1) followed by interested (id = 2)
                    peer.write_all(&[0, 0, 0, 1, 1, 0, 0, 0, 1, 2]).await?;
                    choked = false;
                    println!("----Sent unchoked & interested----");
                }

                if msg == [0u8, 0, 0, 1, 1] && !request {
                    println!("----Peer unchoked us, sending request----");

                    peer.write_all(
                        &[
                            // request (id = 6)
                            &[0u8, 0, 0, 13, 6] as &[u8],
                            &0u32.to_be_bytes(), // 0th piece
                            &0u32.to_be_bytes(), // starting from index 0
                            &2u32.pow(10).to_be_bytes(), // 2**10=1024 bytes or 1KB
                        ]
                        .concat(),
                    )
                    .await?;

                    request = true;
                }
            }
            Err(e) => {
                eprintln!("----failed to read token from socket; err = {:?}----", e);
                break;
            }
        }
    }

    Ok(())
}

fn build_peerlist(response: &[u8]) -> Option<Vec<SocketAddr>> {
    let (_, response_bencode) = parse_bencode(response).ok()?;

    let mut response_dict = response_bencode.dict()?;

    let peer_list = response_dict.remove(b"peers" as &[u8])?;

    match peer_list {
        // compact mode
        Bencode::ByteString(ipv4_peer_bytes) => {
            // This key only exists in compact mode, but some trackers
            // don't support it, so it might not exist even then.
            let ipv6_peer_bytes = response_dict
                .remove(b"peers6" as &[u8])
                .and_then(|val| val.byte_string())
                .unwrap_or_default();

            let (ipv4_chunks, ipv4_remainder) = ipv4_peer_bytes.as_chunks::<6>();
            let (ipv6_chunks, ipv6_remainder) = ipv6_peer_bytes.as_chunks::<18>();

            if !(ipv4_remainder.is_empty() && ipv6_remainder.is_empty()) {
                return None;
            }

            Some(
                ipv4_chunks
                    .iter()
                    .map(|&addr_bytes| {
                        (
                            IpAddr::from(<[u8; 4]>::try_from(&addr_bytes[0..4]).unwrap()),
                            u16::from_be_bytes(addr_bytes[4..].try_into().unwrap()),
                        )
                    })
                    .chain(ipv6_chunks.iter().map(|&addr_bytes| {
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
        Bencode::List(peer_list) => peer_list
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
            .collect::<Option<_>>(),
        _ => None,
    }
}
