#![feature(slice_as_chunks)]

mod bencode_parser;
mod tcp_peer_communicator;
mod torrent_parser;
mod tracker;
mod types;

use bitvec::prelude::*;
use snafu::{ensure, OptionExt, ResultExt, Snafu};
use std::{
    borrow::Cow,
    cmp,
    collections::BTreeMap,
    convert::TryFrom,
    env, error,
    io::SeekFrom,
    mem,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering::*},
        Arc,
    },
    time::Duration,
};
use tcp_peer_communicator::create_tcp_peer_rw;
use tokio::{
    self,
    io::{AsyncSeekExt, AsyncWriteExt},
    net::TcpStream,
    task, time,
};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_peerlist, build_tracker_url};
use types::{Block, BlockMeta, Message, PeerReader, PeerWriter};

const PORT: u16 = 6881;
const KIB: u32 = 1024;
const BLOCK_SIZE: u32 = 16 * KIB;
const MAX_PEERS: usize = 20;
const PIPELINE_AMOUNT: usize = 5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn error::Error>> {
    env_logger::init();

    // Reading torrent file
    let torrent_bytes = std::fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;
    let torrent = Torrent::try_from(torrent_bytes.as_slice()).map_err(|e| {
        log::error!("Torrent parsing error: {:?}", e);

        "Invalid or unsupported torrent file format"
    })?;
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

    // TODO: reannounce to the tracker

    let root_path = Path::new(&torrent.info.name);
    let file_handles = torrent
        .info
        .files
        .iter()
        .map(|file| {
            let file_path = if torrent.info.files.len() == 1 {
                Cow::from(&file.path)
            } else {
                root_path.join(&file.path).into()
            };

            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Using tokio's File::create directly here would be annoying
            // because we'd have to deal with the iterator's Items being Futures
            // instead of actual File values.
            let file_handle = std::fs::File::create(file_path)?;
            file_handle.set_len(file.length)?;

            Ok(tokio::fs::File::from_std(file_handle))
        })
        .collect::<Result<_, std::io::Error>>()?;

    // Shared structures for worker threads
    let torrent = Arc::new(torrent);
    let bitfield_pieces = Arc::new(tokio::sync::RwLock::new(
        bitvec![Msb0, u8; false as u8; torrent.info.pieces.len()],
    ));
    let worker_queue = Arc::new(tokio::sync::RwLock::new(construct_worker_queue(
        &torrent, BLOCK_SIZE,
    )));
    let (blocks_tx, mut blocks_rx) = tokio::sync::mpsc::unbounded_channel::<Block>();
    let (pieces_tx, mut pieces_rx) = tokio::sync::broadcast::channel::<u32>(10);

    let pieces_tx_clone = pieces_tx.clone();
    let torrent_clone = torrent.clone();

    let io_task_handle = task::spawn(async move {
        let result = store_blocks(
            &torrent_clone,
            file_handles,
            &mut blocks_rx,
            &pieces_tx_clone,
        )
        .await;

        if let Err(e) = result {
            log::error!("Error while writing to files: {:?}", e);
        }
    });

    let mut num_peers = 0;

    for peer_addr in peerlist {
        if num_peers == MAX_PEERS {
            break;
        }

        if let Ok(stream) = TcpStream::connect(peer_addr).await {
            if let Ok(peer) =
                create_tcp_peer_rw(stream, torrent.info_hash.as_ref(), peer_id.as_bytes()).await
            {
                log::debug!("Starting connection with {}", peer_addr);

                num_peers += 1;

                let worker_queue = worker_queue.clone();
                let bitfield_pieces = bitfield_pieces.clone();
                let blocks_tx = blocks_tx.clone();

                task::spawn(async move {
                    let result = peer_connection(
                        num_peers,
                        &worker_queue,
                        &bitfield_pieces,
                        &blocks_tx,
                        &mut pieces_rx,
                        peer,
                    )
                    .await;

                    if let Err(e) = result {
                        log::error!("[{}]: Done with error: {:?}", num_peers, e);
                    }
                });
            }
        }

        pieces_rx = pieces_tx.subscribe();
    }

    io_task_handle.await?;

    Ok(())
}

async fn store_blocks(
    torrent: &Torrent,
    mut file_handles: Vec<tokio::fs::File>,
    blocks_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Block>,
    pieces_tx: &tokio::sync::broadcast::Sender<u32>,
) -> Result<(), std::io::Error> {
    assert!(
        torrent.info.files.len() == 1,
        "Multi-file torrents not yet supported"
    );

    while let Some(block) = blocks_rx.recv().await {
        // TODO: SHA-1 verification of a complete piece + notify peers
        log::debug!("Writing block {:?}", block.meta);

        let offset =
            (block.meta.piece_index as u64 * torrent.info.piece_len) + block.meta.begin as u64;

        file_handles[0].seek(SeekFrom::Start(offset)).await?;
        file_handles[0].write_all(&block.data).await?;
    }

    Ok(())
}

async fn peer_connection<R: PeerReader, W: PeerWriter>(
    peer_num: usize,
    worker_queue: &tokio::sync::RwLock<WorkerQueue>,
    bitfield_pieces: &tokio::sync::RwLock<BitVec<Msb0, u8>>,
    blocks_tx: &tokio::sync::mpsc::UnboundedSender<Block>,
    pieces_rx: &mut tokio::sync::broadcast::Receiver<u32>,
    (mut peer_reader, mut peer_writer): (R, W),
) -> Result<(), PeerConnectionError<R::Error, W::Error>>
where
    R::Error: 'static,
    W::Error: 'static,
{
    let peer_pieces =
        tokio::sync::Mutex::new(bitvec![Msb0, u8; false as u8; bitfield_pieces.read().await.len()]);

    let am_choked = AtomicBool::new(true);
    let peer_choked = AtomicBool::new(true);
    let peer_interested = AtomicBool::new(false);

    let block_queue = tokio::sync::RwLock::new(Vec::<(
        tokio::sync::OwnedMutexGuard<BlockMeta>,
        AtomicBool,
    )>::with_capacity(PIPELINE_AMOUNT));

    use Message::*;

    // The Ok's at the end are workarounds to tell Rust what the types for each
    // of these futures will be, since inference with using ? in async blocks is
    // still messed up. See here: https://tinyurl.com/asyncerr
    tokio::try_join!(
        async {
            log::debug!("[{}]: Peer read task started", peer_num);

            loop {
                let message = time::timeout(Duration::from_secs(120), peer_reader.read())
                    .await
                    .context(TimeoutError)?
                    .context(ReadError)?;

                log::debug!("[{}]: Recieved message: {:?}", peer_num, message);

                match message {
                    KeepAlive => continue,
                    Choke => am_choked.store(true, Release),
                    Unchoke => am_choked.store(false, Release),
                    Interested => peer_interested.store(true, Release),
                    NotInterested => peer_interested.store(false, Release),
                    Have(piece_index) => {
                        *peer_pieces
                            .lock()
                            .await
                            .get_mut(piece_index as usize)
                            .context(InvalidPieceIndex)? = true;
                    }
                    BitField(mut bitfield) => {
                        let mut peer_pieces = peer_pieces.lock().await;

                        ensure!(bitfield.len() >= peer_pieces.len(), InvalidBitfield);

                        bitfield.truncate(peer_pieces.len());
                        peer_pieces.swap_with_bitslice(&mut bitfield)
                    }
                    Request(block_meta) => { /* TODO no panic */ }
                    Cancel(block_meta) => { /* TODO no panic */ }
                    Piece(block) => {
                        let block_queue_read = block_queue.read().await;

                        if let Some(idx) =
                            block_queue_read.iter().position(|(b, _)| **b == block.meta)
                        {
                            log::debug!("[{}]: Recieved block {:?}", peer_num, block.meta);

                            drop(block_queue_read);

                            let mut block_queue_write = block_queue.write().await;

                            // Because we're done with this block, we want
                            // it to be effectively permanently locked.
                            // Forgetting the guard will accomplish this
                            // without us having to actually hold onto it.
                            mem::forget(block_queue_write.remove(idx));

                            blocks_tx.send(block).context(BlockSendError)?;
                        } else {
                            log::error!(
                                "[{}]: Recieved block we don't have a lock on: {:?}",
                                peer_num,
                                block.meta
                            );
                        }
                    }
                }
            }

            #[allow(unreachable_code)]
            Ok::<_, PeerConnectionError<R::Error, W::Error>>(())
        },
        async {
            log::debug!("[{}]: Peer write task started", peer_num);

            let mut am_interested = false;
            let mut sent_message = false;
            let mut last_keepalive_instant = time::Instant::now();

            loop {
                let mut block_queue_write = block_queue.write().await;

                if block_queue_write.len() < PIPELINE_AMOUNT {
                    if let Ok(worker_queue_read) =
                        time::timeout(Duration::from_secs(10), worker_queue.read()).await
                    {
                        let peer_pieces_lock = peer_pieces.lock().await;

                        get_available_blocks(
                            &mut block_queue_write,
                            &worker_queue_read,
                            |idx| {
                                match peer_pieces_lock.get(idx as usize) {
                                    Some(is_available) => *is_available,
                                    // This probably should be an error, but we can't
                                    // return it from here.
                                    None => false,
                                }
                            },
                            PIPELINE_AMOUNT,
                        );
                    }
                }

                let block_queue_read = block_queue_write.downgrade();

                if (time::Instant::now() - last_keepalive_instant) > Duration::from_secs(120)
                    && !sent_message
                {
                    peer_writer.write(KeepAlive).await.context(WriteError)?;

                    // We need to send this as soon as possible or we risk
                    // getting disconnected.
                    peer_writer.flush().await.context(WriteError)?;

                    last_keepalive_instant = time::Instant::now();
                    sent_message = false;
                }

                if !am_interested && !block_queue_read.is_empty() {
                    peer_writer.write(Interested).await.context(WriteError)?;

                    am_interested = true;
                    sent_message = true;
                }

                if let Ok(piece_index) = pieces_rx.try_recv() {
                    peer_writer
                        .write(Have(piece_index))
                        .await
                        .context(WriteError)?;

                    sent_message = true;
                }

                // TODO: keep track of if a request has been unfulfilled for too long
                if !am_choked.load(Acquire) {
                    for (block_meta, requested) in block_queue_read.iter() {
                        if !requested.load(Acquire) {
                            log::debug!("[{}]: Requesting {:?}", peer_num, block_meta);

                            peer_writer
                                .write(Request(**block_meta))
                                .await
                                .context(WriteError)?;

                            sent_message = true;
                            requested.store(true, Release);
                        }
                    }
                }

                peer_writer.flush().await.context(WriteError)?;
            }

            #[allow(unreachable_code)]
            Ok::<_, PeerConnectionError<R::Error, W::Error>>(())
        }
    )?;

    Ok(())
}

#[derive(Snafu, Debug)]
enum PeerConnectionError<R: error::Error + 'static, W: error::Error + 'static> {
    ReadError {
        source: R,
    },
    WriteError {
        source: W,
    },
    TimeoutError {
        source: time::error::Elapsed,
    },
    #[snafu(display("Could not send downloaded block through channel: {}", source))]
    BlockSendError {
        source: tokio::sync::mpsc::error::SendError<Block>,
    },
    #[snafu(display("Peer sent message with an invalid piece index"))]
    InvalidPieceIndex,
    #[snafu(display("Peer sent bitfield with incorrect length"))]
    InvalidBitfield,
}

fn get_available_blocks(
    blocks: &mut Vec<(tokio::sync::OwnedMutexGuard<BlockMeta>, AtomicBool)>,
    worker_queue: &WorkerQueue,
    mut peer_has_piece: impl FnMut(u32) -> bool,
    max: usize,
) {
    if blocks.len() == max {
        return;
    }

    for (&PieceKey { piece_index, .. }, piece_work_info) in worker_queue {
        if !peer_has_piece(piece_index) || piece_work_info.available_blocks.load(Acquire) == 0 {
            continue;
        }

        for block_lock in &piece_work_info.blocks {
            if let Ok(block_meta) = block_lock.clone().try_lock_owned() {
                piece_work_info.available_blocks.fetch_sub(1, AcqRel);
                blocks.push((block_meta, AtomicBool::new(false)));

                if blocks.len() == max {
                    return;
                }
            }
        }
    }
}

type WorkerQueue = BTreeMap<PieceKey, PieceWorkInfo>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PieceKey {
    priority: usize,
    piece_index: u32,
}

#[derive(Debug)]
struct PieceWorkInfo {
    blocks: Vec<Arc<tokio::sync::Mutex<BlockMeta>>>,
    // TODO: See if it makes sense to replace this with a Semaphore
    available_blocks: AtomicU32,
}

fn construct_worker_queue(torrent: &Torrent, block_len: u32) -> WorkerQueue {
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

                            Arc::new(tokio::sync::Mutex::new(BlockMeta {
                                piece_index,
                                begin: block_index * block_len,
                                length: this_block_len,
                            }))
                        })
                        .collect(),
                },
            )
        })
        .collect()
}
