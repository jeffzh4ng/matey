#![feature(slice_as_chunks)]

mod bencode_parser;
mod torrent_parser;
mod tracker;
mod types;
mod tcp_peer_communicator;

use std::{convert::TryFrom, env, fs};
use torrent_parser::Torrent;
use tracker::{build_peer_id, build_peerlist, build_tracker_url};
use types::{Message, PeerCommunicator};
use tcp_peer_communicator::TcpPeerCommunicator;

const PORT: u16 = 6881;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let peer_id = build_peer_id();

    log::info!("Our peer ID: {}", peer_id);

    let torrent_bytes = fs::read(
        env::args()
            .nth(1)
            .ok_or("Didn't find a torrent file in the first argument")?,
    )?;

    let torrent = Torrent::try_from(torrent_bytes)?;

    log::info!("Parsed torrent file");

    let tracker_url = build_tracker_url(&torrent, &PORT.to_string(), &peer_id)?;

    log::info!("Announcing to tracker at URL: {}", tracker_url);

    let resp = reqwest::get(tracker_url).await?;

    log::info!("Got response from tracker");

    let peerlist = build_peerlist(&resp.bytes().await?)
        .ok_or("Failed to find valid peerlist in tracker response")?;

    log::info!("Parsed peerlist from tracker response");

    // This attempts a connection with each peer in turn until one succeeds.
    let mut peer = TcpPeerCommunicator::new(
        peerlist.as_slice(),
        torrent.info_hash.as_ref(),
        peer_id.as_bytes(),
    )
    .await?;

    log::info!("Established BitTorrent connection with peer");

    let mut choked = true;
    let mut request = false;

    loop {
        match peer.read().await {
            Ok(Some(msg)) => {
                if let Message::Piece {
                    index,
                    begin,
                    block,
                } = &msg
                {
                    log::info!(
                        "Recieved piece {} at offset {}:\n {:?}",
                        index,
                        begin,
                        String::from_utf8_lossy(block)
                    );
                } else {
                    log::info!("Recieved message from peer: {:?}", &msg);
                }

                if choked {
                    // TODO: Provide a way to pipeline writes? Builder pattern, maybe?
                    peer.write(Message::Unchoke).await?;
                    peer.write(Message::Interested).await?;

                    choked = false;

                    log::info!("Sent unchoke and interested to peer");
                }

                if msg == Message::Unchoke && !request {
                    log::info!("Peer unchoked us, sending request");

                    peer.write(Message::Request {
                        index: 0,
                        begin: 0,
                        length: 2u32.pow(10), // 2**10 = 1024 bytes or 1KB
                    })
                    .await?;

                    request = true;
                }
            }
            Ok(None) => {
                log::debug!("Peer sent keep-alive message");
            }
            Err(e) => {
                log::error!("failed to read token from socket; err = {:?}", e);
                break;
            }
        }
    }

    Ok(())
}
