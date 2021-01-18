#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::build_tracker_url;

use bencode_parser::{parse_bencode, Bencode};
use std::{
    convert::TryInto,
    net::{IpAddr, SocketAddr},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;

    let torrent = Torrent::try_from(torrent_bytes)?;

    let tracker_url = build_tracker_url(&torrent, "6881")?;

    let resp = reqwest::get(tracker_url).await?;

    dbg!(build_peerlist(&resp.bytes().await?));

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
