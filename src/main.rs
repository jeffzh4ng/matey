#![feature(slice_as_chunks)]

mod bencode_parser;
mod tcp_peer_communicator;
mod torrent_parser;
mod tracker;
mod types;

use bitvec::prelude::*;
use std::{
    cmp,
    collections::BTreeMap,
    convert::TryFrom,
    env, fs,
    sync::{
        atomic::{AtomicBool, AtomicU32},
        Arc,
    },
};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_peerlist, build_tracker_url};
use types::BlockMeta;

const PORT: u16 = 6881;
const KB: u32 = 1024;
const BLOCK_SIZE: u32 = 16 * KB;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // Reading torrent file
    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;
    let torrent = Torrent::try_from(torrent_bytes)?;
    log::info!("Parsed torrent file");

    // Tracker networking
    let peer_id = build_peer_id();
    log::info!("Our peer ID: {}", peer_id);
    let tracker_url = build_tracker_url(&torrent, &PORT.to_string(), &peer_id)?;
    log::info!("Announcing to tracker at URL: {}", tracker_url);
    let resp = reqwest::get(tracker_url).await?;
    log::info!("Got response from tracker");
    let peerlist = build_peerlist(&resp.bytes().await?)
        .ok_or("Failed to find valid peerlist in tracker response")?;
    log::info!("Parsed peerlist from tracker response");

    // Shared structures for worker threads
    let bitfield_pieces = Arc::new(tokio::sync::RwLock::new(
        bitvec![Msb0, u8; false as u8; torrent.info.pieces.len()],
    ));
    let worker_queue = Arc::new(tokio::sync::RwLock::new(construct_worker_queue(
        &torrent, BLOCK_SIZE,
    )));
    let (results_tx, mut results_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (pieces_tx, mut pieces_rx) = tokio::sync::broadcast::channel::<u32>(10);

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PieceKey {
    priority: usize,
    piece_index: u32,
}

#[derive(Debug)]
struct PieceWorkInfo {
    blocks: Vec<BlockWorkInfo>,
    available_blocks: AtomicU32,
}

#[derive(Debug)]
struct BlockWorkInfo {
    downloaded: AtomicBool,
    meta: tokio::sync::Mutex<BlockMeta>,
}

fn construct_worker_queue(torrent: &Torrent, block_len: u32) -> BTreeMap<PieceKey, PieceWorkInfo> {
    assert!(
        torrent.info.piece_len >= block_len as u64,
        "Piece length is smaller than the block length"
    );

    let mut left_bytes = torrent.info.files.iter().map(|f| f.length).sum::<u64>();

    (0..torrent.info.pieces.len() as u32)
        .map(|piece_index| {
            let mut left_piece_len = cmp::min(left_bytes, torrent.info.piece_len);
            left_bytes = left_bytes.saturating_sub(torrent.info.piece_len);

            let num_blocks = (left_piece_len as f64 / block_len as f64).ceil() as u32;

            (
                PieceKey {
                    priority: 0,
                    piece_index,
                },
                PieceWorkInfo {
                    available_blocks: AtomicU32::new(num_blocks),
                    blocks: (0..num_blocks)
                        .map(|block_index| {
                            let this_block_len = cmp::min(left_piece_len, block_len as u64) as u32;
                            left_piece_len = left_piece_len.saturating_sub(block_len as u64);

                            BlockWorkInfo {
                                downloaded: AtomicBool::new(false),
                                meta: tokio::sync::Mutex::new(BlockMeta {
                                    piece_index,
                                    begin: block_index * block_len,
                                    length: this_block_len,
                                }),
                            }
                        })
                        .collect(),
                },
            )
        })
        .collect()
}
