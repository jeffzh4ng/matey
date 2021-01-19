use super::torrent_parser::Torrent;
use build_info;
use percent_encoding::{percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::Url;
use std::time::SystemTime;

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
    let version_str = format!(
        "{:0^4}",
        build_info::format!("{}", $.crate_info.version)
            .to_owned()
            .replace(".", "")
    );

    let mut time_str = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string();

    time_str.truncate(12);

    let client_id = "MS"; // the Matey Ship! 🏴‍☠️

    format!("-{}{}-{:0^12}", client_id, version_str, time_str)
}
