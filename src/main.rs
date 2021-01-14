#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::build_tracker_url;

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

    dbg!(resp.bytes().await?);

    Ok(())
}
