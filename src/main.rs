#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;

use bencode_parser::parse_bencode;
use torrent_parser::Torrent;
use std::{convert::TryFrom, env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;
    let (_, torrent_bencode) = parse_bencode(&torrent_bytes).map_err(|_| "Invalid bencode")?;
    let torrent = Torrent::try_from(torrent_bencode)?;

    dbg!(torrent);

    Ok(())
}
