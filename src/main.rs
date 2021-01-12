mod bencode_parser;

use std::{env, fs, path::PathBuf};
use snafu::Snafu;
use bencode_parser::{Bencode, parse_bencode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let torrent_bytes = fs::read(env::args().nth(1).ok_or("Didn't find a torrent file in the first argument")?)?;

    let (_, torrent) = parse_bencode(&torrent_bytes).map_err(|_| "Invalid bencode")?;

    parse_torrent(torrent)?;

    Ok(())
}

fn parse_torrent(torrent: Bencode) -> Result<Torrent, TorrentParsingError> {
    let torrent_dict = if let Bencode::Dict(dict) = torrent { dict } else {
        return Err(TorrentParsingError::NotADictionary);
    };

    todo!()
}

pub struct Torrent {
    announce: String,
    encoding: Option<String>,
    info: TorrentInfo,
}

type SHA1Hash = [u8; 20];

pub struct TorrentInfo {
    piece_len: u64,
    pieces: Vec<SHA1Hash>,
    name: String,
    files: Vec<TorrentFile>,
}

pub struct TorrentFile {
    length: u64,
    path: PathBuf,
}

#[non_exhaustive]
#[derive(Debug, Snafu)]
pub enum TorrentParsingError {
    #[snafu(display("Torrent bencode is not a dictionary"))]
    NotADictionary,
}