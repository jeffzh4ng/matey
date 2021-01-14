#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::build_tracker_url;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;

    let torrent = Torrent::try_from(torrent_bytes)?;

    let url = build_tracker_url(&torrent, "6881")?;

    dbg!(url.as_str());
    
    Ok(())
}