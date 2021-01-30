use super::bencode_parser::{parse_bencode, Bencode};
use super::torrent_parser::Torrent;
use bytes::Bytes;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use rand::{distributions, thread_rng, Rng};
use reqwest::Url;
use std::{
    convert::{TryFrom, TryInto},
    net::{IpAddr, SocketAddr},
};

const NEEDS_ESCAPE_BYTES: AsciiSet = NON_ALPHANUMERIC
    .remove(b'.')
    .remove(b'-')
    .remove(b'_')
    .remove(b'~');

pub fn build_tracker_url(
    torrent: &Torrent,
    port: &str,
    peer_id: &str,
) -> Result<Url, Box<dyn std::error::Error>> {
    let mut url = Url::parse(&torrent.announce)?;

    url.set_query(Some(&format!(
        "info_hash={}",
        &percent_encode(torrent.info_hash.as_ref(), &NEEDS_ESCAPE_BYTES)
    )));

    url.query_pairs_mut()
        .append_pair("port", port)
        .append_pair("uploaded", "0")
        .append_pair("downloaded", "0")
        .append_pair("compact", "1")
        .append_pair("event", "started")
        .append_pair(
            "left",
            &torrent
                .info
                .files
                .iter()
                .map(|f| f.length)
                .sum::<u64>()
                .to_string(),
        )
        .append_pair("peer_id", peer_id);

    Ok(url)
}

pub fn build_peer_id() -> String {
    let client_id = "MS"; // the Matey Ship! 🏴‍☠️

    let mut version_str = env!("CARGO_PKG_VERSION").to_owned().replace(".", "");

    version_str.truncate(4);

    let suffix = thread_rng()
        .sample_iter(&distributions::Alphanumeric)
        .take(12)
        .map(char::from)
        .collect::<String>();

    format!("-{}{:0>4}-{}", client_id, version_str, suffix)
}

pub fn build_peerlist(response: &[u8]) -> Option<Vec<SocketAddr>> {
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
        peers.unwrap_or_else(|| Bencode::ByteString(Bytes::new())),
        v6_peers.unwrap_or_else(|| Bencode::ByteString(Bytes::new())),
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
