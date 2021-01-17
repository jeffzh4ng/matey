use super::bencode_parser::{self, parse_bencode, Bencode};
use nom::{
    bytes::complete::{tag, take_until},
    combinator::recognize,
    error::ErrorKind,
};
use sha1::{Digest, Sha1};
use snafu::{ensure, OptionExt, ResultExt, Snafu};
use std::{
    convert::{TryFrom, TryInto},
    fmt, num,
    path::PathBuf,
    string,
};

#[derive(Clone, Debug)]
pub struct Torrent {
    pub announce: String,
    pub info: TorrentInfo,
    pub info_hash: SHA1Hash,
}

impl TryFrom<Vec<u8>> for Torrent {
    type Error = TorrentParsingError;

    fn try_from(torrent_bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let mut torrent_dict = parse_bencode(&torrent_bytes)
            .map_err(|_| TorrentParsingError::InvalidBencode)
            .and_then(|(_, bencode)| bencode.dict().context(NotADict))?;

        let announce = torrent_dict
            .remove(b"announce" as &[u8])
            .and_then(|val| val.byte_string())
            .context(FieldNotFound { field: "announce" })
            .and_then(|val| String::from_utf8(val).context(InvalidString))?;

        let info = torrent_dict
            .remove(b"info" as &[u8])
            .context(FieldNotFound { field: "info" })
            .and_then(TorrentInfo::try_from)?;

        let (bytes_after_info_token, _) =
            // Rust cannot infer an error type by default, so we use nom's
            // usual (Input, ErrorKind) type. See the nom docs for details.
            take_until::<_, _, (_, ErrorKind)>("info")(torrent_bytes.as_slice())
                // take_until doesn't consume the pattern itself,
                // so we have to get rid of that part. It's guaranteed
                // to be there, so we can just unwrap this.
                .map(|(bytes, _)| tag::<_, _, (_, ErrorKind)>("info")(bytes).unwrap())
                .map_err(|_| TorrentParsingError::InvalidBencode)?;

        let (_, info_bytes) = recognize(bencode_parser::dict)(bytes_after_info_token).unwrap();

        let info_hash = SHA1Hash(Sha1::digest(info_bytes).as_slice().try_into().unwrap());

        Ok(Self {
            announce,
            info,
            info_hash,
        })
    }
}

#[derive(Clone, Debug)]
pub struct TorrentInfo {
    pub name: String,
    pub files: Vec<TorrentFile>,
    pub piece_len: u64,
    pub pieces: Vec<SHA1Hash>,
}

impl TryFrom<Bencode> for TorrentInfo {
    type Error = TorrentParsingError;

    fn try_from(info_bencode: Bencode) -> Result<Self, Self::Error> {
        let mut torrent_info_dict = info_bencode.dict().context(NotADict)?;

        let name = torrent_info_dict
            .remove(b"name" as &[u8])
            .and_then(|val| val.byte_string())
            .context(FieldNotFound {
                field: "info[name]",
            })
            .and_then(|val| String::from_utf8(val).context(InvalidString))?;

        let files = if let Some(multiple_files) = torrent_info_dict
            .remove(b"files" as &[u8])
            .and_then(|val| val.list())
        {
            multiple_files
                .into_iter()
                .map(TorrentFile::try_from)
                .collect::<Result<_, _>>()?
        } else {
            vec![TorrentFile {
                length: torrent_info_dict
                    .remove(b"length" as &[u8])
                    .and_then(|val| val.number())
                    .context(FieldNotFound {
                        field: "info[length]",
                    })
                    .and_then(|val| u64::try_from(val).context(InvalidFileLen))?,
                path: name.clone().into(),
            }]
        };

        let piece_len = torrent_info_dict
            .remove(b"piece length" as &[u8])
            .and_then(|val| val.number())
            .context(FieldNotFound {
                field: "info[piece length]",
            })
            .and_then(|val| u64::try_from(val).context(InvalidPieceLen))?;

        let all_pieces = torrent_info_dict
            .remove(b"pieces" as &[u8])
            .and_then(|val| val.byte_string())
            .context(FieldNotFound {
                field: "info[pieces]",
            })?;

        let (pieces, remainder) = all_pieces.as_chunks();

        ensure!(remainder.is_empty(), MismatchedPieceLength);

        let pieces = pieces.iter().copied().map(SHA1Hash).collect();

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
    pub length: u64,
    pub path: PathBuf,
}

impl TryFrom<Bencode> for TorrentFile {
    type Error = TorrentParsingError;

    fn try_from(file_bencode: Bencode) -> Result<Self, Self::Error> {
        let mut file_dict = file_bencode.dict().context(NotADict)?;

        let length = file_dict
            .remove(b"length" as &[u8])
            .and_then(|val| val.number())
            .context(FieldNotFound {
                field: "file[length]",
            })
            .and_then(|val| u64::try_from(val).context(InvalidFileLen))?;

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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct SHA1Hash([u8; 20]);

impl AsRef<[u8; 20]> for SHA1Hash {
    fn as_ref(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Debug for SHA1Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }

        Ok(())
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
    #[snafu(display("Provided bytes aren't valid bencode"))]
    InvalidBencode,
}
