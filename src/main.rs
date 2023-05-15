#![feature(slice_as_chunks)]

mod bencode_parser;
mod tcp_peer_communicator;
mod torrent_parser;
mod tracker;
mod types;

use bitvec::prelude::*;
use bytes::BytesMut;
use indicatif::{ProgressBar, ProgressStyle};
use pin_project_lite::pin_project;
use rand::{distributions::Bernoulli, prelude::*};
use sha1::{Digest, Sha1};
use snafu::{ensure, OptionExt, ResultExt, Snafu};
use std::{
    borrow::{Borrow, Cow},
    cmp,
    collections::BTreeMap,
    convert::{TryFrom, TryInto},
    env, error,
    io::{self, SeekFrom},
    mem,
    path::Path,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering::*},
        Arc,
    },
    task::{ready, Context, Poll},
    time::Duration,
};
use tcp_peer_communicator::create_tcp_peer_rw;
use tokio::{
    self,
    io::{AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::{broadcast, mpsc, Mutex, OwnedMutexGuard, RwLock},
    task, time,
};
use torrent_parser::{SHA1Hash, Torrent};
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
            let file_handle = std::fs::File::options()
                .read(true)
                .write(true)
                .create(true)
                .open(file_path)?;

            file_handle.set_len(file.length)?;

            Ok(tokio::fs::File::from_std(file_handle))
        })
        .collect::<Result<_, std::io::Error>>()?;

    // Shared structures for worker threads
    let torrent = Arc::new(torrent);
    let bitfield_pieces = Arc::new(RwLock::new(
        bitvec![Msb0, u8; false as u8; torrent.info.pieces.len()],
    ));
    let worker_queue = Arc::new(RwLock::new(construct_worker_queue(&torrent, BLOCK_SIZE)));
    let (blocks_tx, mut blocks_rx) = mpsc::channel::<Block>(50);
    let (pieces_tx, mut pieces_rx) = broadcast::channel::<(u32, usize)>(10);

    let pieces_tx_clone = pieces_tx.clone();
    let torrent_clone = torrent.clone();

    task::spawn(async move {
        let result = store_blocks(
            &torrent_clone,
            file_handles,
            &mut blocks_rx,
            &pieces_tx_clone,
        )
        .await;

        if let Err(e) = result {
            log::error!("Error while doing disk I/O: {:?}", e);
        }
    });

    let pb = ProgressBar::new(torrent.info.files.iter().map(|f| f.length).sum());

    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed}] {percent}% [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}) (ETA: {eta})")
        .progress_chars("#>-"));

    pb.enable_steady_tick(50);

    let bitfield_pieces_clone = bitfield_pieces.clone();
    let worker_queue_clone = worker_queue.clone();

    let progress_task_handle = task::spawn(async move {
        while let Ok((piece_index, piece_len)) = pieces_rx.recv().await {
            pb.inc(piece_len as u64);

            *bitfield_pieces_clone
                .write()
                .await
                .get_mut(piece_index as usize)
                .unwrap() = true;

            let mut worker_queue = worker_queue_clone.write().await;
            worker_queue.remove(&piece_index);

            if worker_queue.downgrade().is_empty() {
                break;
            }
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
                pieces_rx = pieces_tx.subscribe();

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
                        log::warn!("[{}]: Done with error: {:?}", num_peers, e);
                    }
                });
            }
        }
    }

    progress_task_handle.await?;

    Ok(())
}

async fn store_blocks(
    torrent: &Torrent,
    mut file_handles: Vec<tokio::fs::File>,
    blocks_rx: &mut mpsc::Receiver<Block>,
    pieces_tx: &broadcast::Sender<(u32, usize)>,
) -> Result<(), std::io::Error> {
    assert!(
        torrent.info.files.len() == 1,
        "Multi-file torrents not yet supported"
    );

    let mut buf = BytesMut::with_capacity(torrent.info.piece_len as usize);

    while let Some(block) = blocks_rx.recv().await {
        log::debug!("Writing block {:?}", block.meta);

        let piece_offset = block.meta.piece_index as u64 * torrent.info.piece_len;

        file_handles[0]
            .seek(SeekFrom::Start(piece_offset + block.meta.begin as u64))
            .await?;
        file_handles[0].write_all(&block.data).await?;
        file_handles[0].sync_data().await?;

        file_handles[0].seek(SeekFrom::Start(piece_offset)).await?;

        buf.clear();

        while file_handles[0].read_buf(&mut buf).await? > 0 {
            if buf.len() == torrent.info.piece_len as usize {
                break;
            }
        }

        if SHA1Hash(Sha1::digest(&buf).try_into().unwrap())
            == torrent.info.pieces[block.meta.piece_index as usize]
        {
            log::debug!("Wrote complete piece {}", block.meta.piece_index);

            // We don't really care if this is failed to send right now, the
            // docs say that this doesn't mean future calls to send will fail.
            let _ = pieces_tx.send((block.meta.piece_index, buf.len()));
        }
    }

    Ok(())
}

pin_project! {
    #[derive(Debug)]
    struct FixedLengthChain<F, S> {
        #[pin]
        first: F,
        #[pin]
        second: S,
        first_cursor: usize,
        first_total: usize,
    }
}

impl<F, S> FixedLengthChain<F, S> {
    fn new(first: F, second: S, first_total: usize) -> Self {
        Self {
            first,
            second,
            first_cursor: 0,
            first_total,
        }
    }
}

impl<F: AsyncRead, S: AsyncRead> AsyncRead for FixedLengthChain<F, S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let me = self.project();

        if me.first_cursor < me.first_total {
            let rem = buf.remaining();
            let max_new_bytes = me.first_total.saturating_sub(*me.first_cursor);

            ready!(me.first.poll_read(cx, buf))?;

            let new_bytes = rem.saturating_sub(buf.remaining());

            if new_bytes > max_new_bytes {
                buf.set_filled(buf.filled().len() - (new_bytes - max_new_bytes));
            }

            *me.first_cursor += new_bytes;

            if me.first_cursor < me.first_total {
                return Poll::Ready(Ok(()));
            }
        }

        me.second.poll_read(cx, buf)
    }
}

impl<F: AsyncWrite, S: AsyncWrite> AsyncWrite for FixedLengthChain<F, S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        let me = self.project();

        let remaining_bytes = me.first_total.saturating_sub(*me.first_cursor);

        let (first_buf, second_buf) = buf.split_at(cmp::min(remaining_bytes, buf.len()));

        let mut written = 0;

        if !first_buf.is_empty() {
            written += ready!(me.first.poll_write(cx, first_buf))?;
        }

        if !second_buf.is_empty() {
            written += ready!(me.second.poll_write(cx, second_buf))?;
        }

        Poll::Ready(Ok(written))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        let me = self.project();

        ready!(me.first.poll_flush(cx))?;
        me.second.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        let me = self.project();

        ready!(me.first.poll_shutdown(cx))?;
        me.second.poll_shutdown(cx)
    }
}

impl<F: AsyncSeek, S: AsyncSeek> AsyncSeek for FixedLengthChain<F, S> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        todo!()
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<u64>> {
        todo!()
    }
}

async fn peer_connection<R: PeerReader, W: PeerWriter>(
    peer_num: usize,
    worker_queue: &RwLock<WorkerQueue>,
    bitfield_pieces: &RwLock<BitVec<Msb0, u8>>,
    blocks_tx: &mpsc::Sender<Block>,
    pieces_rx: &mut broadcast::Receiver<(u32, usize)>,
    (mut peer_reader, mut peer_writer): (R, W),
) -> Result<(), PeerConnectionError<R::Error, W::Error>>
where
    R::Error: 'static,
    W::Error: 'static,
{
    let peer_pieces =
        Mutex::new(bitvec![Msb0, u8; false as u8; bitfield_pieces.read().await.len()]);

    let am_choked = AtomicBool::new(true);
    let peer_interested = AtomicBool::new(false);

    let block_queue = RwLock::new(Vec::<BlockWork>::with_capacity(PIPELINE_AMOUNT));

    // Unwrap is infallible here because from_ratio only errors when numerator >
    // denominiator or denominator == 0.
    let have_rate = Bernoulli::from_ratio(1, 5).unwrap();

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
                    Choke => {
                        am_choked.store(true, Release);

                        // Unfulfilled requests should be considered dropped when choked.
                        block_queue
                            .write()
                            .await
                            .retain(|BlockWork { requested, .. }| !requested.load(Acquire));
                    }
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
                    Request(_block_meta) => { /* TODO no panic */ }
                    Cancel(_block_meta) => { /* TODO no panic */ }
                    Piece(received_block) => {
                        let block_queue_read = block_queue.read().await;

                        if let Some(idx) =
                            block_queue_read
                                .iter()
                                .position(|BlockWork { block_meta, .. }| {
                                    **block_meta == received_block.meta
                                })
                        {
                            log::debug!("[{}]: Recieved block {:?}", peer_num, received_block.meta);

                            drop(block_queue_read);

                            let mut block_queue_write = block_queue.write().await;

                            // Because we're done with this block, we want
                            // it to be effectively permanently locked.
                            // Forgetting the guard will accomplish this
                            // without us having to actually hold onto it.
                            mem::forget(block_queue_write.remove(idx));

                            blocks_tx
                                .send(received_block)
                                .await
                                .context(BlockSendError)?;
                        } else {
                            log::warn!(
                                "[{}]: Recieved block we don't have a lock on: {:?}",
                                peer_num,
                                received_block.meta
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

            let mut need_send_bitfield = true;
            let mut am_interested = false;
            let mut sent_message = false;
            let mut last_keepalive_instant = time::Instant::now();

            loop {
                if need_send_bitfield {
                    let bitfield = bitfield_pieces.read().await;

                    if bitfield.iter().by_ref().any(|&have_piece| have_piece) {
                        log::debug!("[{}]: Sending bitfield", peer_num);

                        peer_writer
                            .write(BitField(bitfield.clone()))
                            .await
                            .context(WriteError)?;
                    }

                    need_send_bitfield = false;
                }

                let mut block_queue_write = block_queue.write().await;

                if block_queue_write.len() < PIPELINE_AMOUNT {
                    if let Ok(worker_queue_read) =
                        time::timeout(Duration::from_secs(10), worker_queue.read()).await
                    {
                        let peer_pieces_lock = peer_pieces.lock().await;

                        get_available_blocks(
                            &mut block_queue_write,
                            &worker_queue_read,
                            |idx| peer_pieces_lock.get(idx as usize).as_deref() == Some(&true),
                            PIPELINE_AMOUNT,
                        );
                    }
                }

                let block_queue_read = block_queue_write.downgrade();

                if (time::Instant::now() - last_keepalive_instant) > Duration::from_secs(120)
                    && !sent_message
                {
                    log::debug!("[{}]: Sending keepalive", peer_num);

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

                if let Ok((piece_index, _)) = pieces_rx.try_recv() {
                    let peer_has_piece = peer_pieces
                        .lock()
                        .await
                        .get(piece_index as usize)
                        .as_deref()
                        == Some(&true);

                    // A peer is extremely unlikely to want to download a piece
                    // they already have, so supressing HAVE messages in that
                    // case reduces overhead. However, it does help the swarm
                    // determine pieces that are rare, so I chose to have it be
                    // based on random chance.
                    if !peer_has_piece || have_rate.sample(&mut rand::thread_rng()) {
                        log::debug!("[{}]: Sending HAVE {}", peer_num, piece_index);

                        peer_writer
                            .write(Have(piece_index))
                            .await
                            .context(WriteError)?;

                        sent_message = true;
                    }
                }

                // TODO: keep track of if a request has been unfulfilled for too long
                if !am_choked.load(Acquire) {
                    for BlockWork {
                        block_meta,
                        requested,
                        ..
                    } in block_queue_read.iter()
                    {
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
        source: mpsc::error::SendError<Block>,
    },
    #[snafu(display("Peer sent message with an invalid piece index"))]
    InvalidPieceIndex,
    #[snafu(display("Peer sent bitfield with incorrect length"))]
    InvalidBitfield,
}

fn get_available_blocks(
    blocks: &mut Vec<BlockWork>,
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

                blocks.push(BlockWork {
                    block_meta,
                    requested: AtomicBool::new(false),
                    piece_available_blocks: piece_work_info.available_blocks.clone(),
                });

                if blocks.len() == max {
                    return;
                }
            }
        }
    }
}

struct BlockWork {
    block_meta: OwnedMutexGuard<BlockMeta>,
    requested: AtomicBool,
    piece_available_blocks: Arc<AtomicU32>,
}

impl Drop for BlockWork {
    fn drop(&mut self) {
        self.piece_available_blocks.fetch_add(1, AcqRel);
    }
}

type WorkerQueue = BTreeMap<PieceKey, PieceWorkInfo>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PieceKey {
    // TODO: find a way to cleanly implement priority
    piece_index: u32,
}

impl Borrow<u32> for PieceKey {
    fn borrow(&self) -> &u32 {
        &self.piece_index
    }
}

#[derive(Debug)]
struct PieceWorkInfo {
    blocks: Vec<Arc<Mutex<BlockMeta>>>,
    available_blocks: Arc<AtomicU32>,
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
                PieceKey { piece_index },
                PieceWorkInfo {
                    available_blocks: Arc::new(AtomicU32::new(num_blocks)),
                    blocks: (0..num_blocks)
                        .map(|block_index| {
                            let this_block_len = cmp::min(left_piece_len, block_len as u64) as u32;
                            left_piece_len = left_piece_len.saturating_sub(block_len as u64);

                            Arc::new(Mutex::new(BlockMeta {
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
