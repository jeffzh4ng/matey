use super::bencode_parser::Bencode;
use snafu::{ensure, OptionExt, ResultExt, Snafu};
use std::{convert::TryFrom, fmt, num, path::PathBuf, string};

#[derive(Clone, Debug)]
pub struct Torrent {
    announce: String,
    info: TorrentInfo,
}

impl TryFrom<Bencode> for Torrent {
    type Error = TorrentParsingError;

    fn try_from(torrent_bencode: Bencode) -> Result<Self, Self::Error> {
        let mut torrent_dict = torrent_bencode
            .dict()
            .ok_or(TorrentParsingError::NotADict)?;

        let announce = String::from_utf8(
            torrent_dict
                .remove(b"announce" as &[u8])
                .and_then(|val| val.byte_string())
                .context(FieldNotFound { field: "announce" })?,
        )
        .context(InvalidString)?;

        let info = TorrentInfo::try_from(
            torrent_dict
                .remove(b"info" as &[u8])
                .context(FieldNotFound { field: "info" })?,
        )?;

        Ok(Self { announce, info })
    }
}

#[derive(Clone, Copy)]
struct SHA1Hash([u8; 20]);

impl fmt::Debug for SHA1Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct TorrentInfo {
    name: String,
    files: Vec<TorrentFile>,
    piece_len: u64,
    pieces: Vec<SHA1Hash>,
}

// TODO: single file mode
impl TryFrom<Bencode> for TorrentInfo {
    type Error = TorrentParsingError;

    fn try_from(info_bencode: Bencode) -> Result<Self, Self::Error> {
        let mut torrent_info_dict = info_bencode.dict().ok_or(TorrentParsingError::NotADict)?;

        let name = String::from_utf8(
            torrent_info_dict
                .remove(b"name" as &[u8])
                .and_then(|val| val.byte_string())
                .context(FieldNotFound {
                    field: "info[name]",
                })?,
        )
        .context(InvalidString)?;

        let files = torrent_info_dict
            .remove(b"files" as &[u8])
            .and_then(|val| val.list())
            .context(FieldNotFound {
                field: "info[files]",
            })?
            .into_iter()
            .map(TorrentFile::try_from)
            .collect::<Result<_, _>>()?;

        let piece_len = u64::try_from(
            torrent_info_dict
                .remove(b"piece length" as &[u8])
                .and_then(|val| val.number())
                .context(FieldNotFound {
                    field: "info[piece length]",
                })?,
        )
        .context(InvalidPieceLen)?;

        let all_pieces = torrent_info_dict
            .remove(b"pieces" as &[u8])
            .and_then(|val| val.byte_string())
            .context(FieldNotFound {
                field: "info[pieces]",
            })?;

        let (pieces, remainder) = all_pieces.as_chunks();

        ensure!(remainder.is_empty(), MismatchedPieceLength);

        let pieces = pieces
            .iter()
            .map(|&hash_bytes| SHA1Hash(hash_bytes))
            .collect();

        Ok(Self {
            name,
            files,
            piece_len,
            pieces,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TorrentFile {
    length: u64,
    path: PathBuf,
}

impl TryFrom<Bencode> for TorrentFile {
    type Error = TorrentParsingError;

    fn try_from(file_bencode: Bencode) -> Result<Self, Self::Error> {
        let mut file_dict = file_bencode.dict().ok_or(TorrentParsingError::NotADict)?;

        let length = u64::try_from(
            file_dict
                .remove(b"length" as &[u8])
                .and_then(|val| val.number())
                .context(FieldNotFound {
                    field: "file[length]",
                })?,
        )
        .context(InvalidFileLen)?;

        let path = file_dict
            .remove(b"path" as &[u8])
            .and_then(|val| val.list())
            .context(FieldNotFound {
                field: "file[path]",
            })?
            .into_iter()
            .map(|val| {
                String::from_utf8(val.byte_string().context(InvalidPath)?).context(InvalidString)
            })
            .collect::<Result<_, _>>()?;

        Ok(Self { length, path })
    }
}

#[non_exhaustive]
#[derive(Debug, Snafu)]
pub enum TorrentParsingError {
    #[snafu(display("Expected a dictionary, but didn't find it"))]
    NotADict,
    #[snafu(display("Attempted to decode an invalid string"))]
    InvalidString { source: string::FromUtf8Error },
    #[snafu(display("Couldn't find field {}", field))]
    FieldNotFound { field: String },
    #[snafu(display("Invalid piece length"))]
    InvalidPieceLen { source: num::TryFromIntError },
    #[snafu(display("Invalid file length"))]
    InvalidFileLen { source: num::TryFromIntError },
    #[snafu(display("Invalid file path: not a list of strings"))]
    InvalidPath,
    #[snafu(display("Found a piece with length < 20"))]
    MismatchedPieceLength,
}