// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::{App, protocol};
use anyhow::{Result, bail};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, lookup_host},
    sync::mpsc,
};

pub const MAGIC: &[u8; 6] = b"CSQPX2";
const HEADER_LEN: usize = MAGIC.len() + 1 + 8;
const MAX_DATA: usize = 1_280;
use crate::proxy_sequence::StreamSequence;
const MAX_GLOBAL_STREAMS: usize = 1024;
const MAX_SESSION_STREAMS: usize = 256;
/// How long a CSQPX2 reorder hole may stay open before the stream is reset.
const DATA_HOLE_PATIENCE: Duration = Duration::from_secs(6);
/// Absolute quiet timeout for the whole stream (both directions idle). Bounds
/// ghosts left by failed open attempts and targets that silently stopped.
const DATA_QUIET_TIMEOUT: Duration = Duration::from_secs(30);
const OPEN: u8 = 1;
const OPEN_OK: u8 = 2;
const OPEN_ERR: u8 = 3;
const DATA: u8 = 4;
const CLOSE: u8 = 5;
const RESEND: u8 = 6;
/// How much of the outbound plaintext tail each stream keeps so a client
/// RESEND request (a lost chunk that the duplicated copies failed to heal)
/// can be replayed. Matches the client reorder window size.
const RESEND_BUFFER_BYTES: usize = 128 * 1024;
/// Upper bound of chunks replayed per single RESEND request, so one request
/// cannot flood the tunnel after a large loss burst.
const RESEND_MAX_FRAMES: usize = 128;

#[derive(Debug)]
pub enum StreamInput {
    Data(Vec<u8>),
    Resend(u64),
    Close,
}

struct Frame<'a> {
    kind: u8,
    stream_id: u64,
    payload: &'a [u8],
}

pub fn is_frame(payload: &[u8]) -> bool {
    payload.len() >= HEADER_LEN && payload.starts_with(MAGIC)
}

fn parse_frame(payload: &[u8]) -> Option<Frame<'_>> {
    if !is_frame(payload) {
        return None;
    }
    Some(Frame {
        kind: payload[6],
        stream_id: u64::from_be_bytes(payload[7..15].try_into().ok()?),
        payload: &payload[HEADER_LEN..],
    })
}

fn encode_frame(kind: u8, stream_id: u64, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.push(kind);
    frame.extend_from_slice(&stream_id.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn parse_target(payload: &[u8]) -> Result<(String, u16)> {
    let (&kind, rest) = payload
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("missing address type"))?;
    let (host, port_bytes) = match kind {
        1 if rest.len() == 6 => (
            std::net::Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3]).to_string(),
            &rest[4..],
        ),
        4 if rest.len() == 18 => (
            std::net::Ipv6Addr::from(<[u8; 16]>::try_from(&rest[..16])?).to_string(),
            &rest[16..],
        ),
        3 if !rest.is_empty() => {
            let length = rest[0] as usize;
            if length == 0 || rest.len() != 1 + length + 2 {
                bail!("invalid domain target");
            }
            let host = std::str::from_utf8(&rest[1..1 + length])?.to_owned();
            if host.chars().any(char::is_control) {
                bail!("invalid domain target");
            }
            (host, &rest[1 + length..])
        }
        _ => bail!("unsupported target address"),
    };
    let port = u16::from_be_bytes(port_bytes.try_into()?);
    if port == 0 {
        bail!("invalid target port");
    }
    Ok((host, port))
}

/// Whether a resolved target may be dialled.
///
/// Loopback, unspecified, multicast and broadcast stay blocked unconditionally:
/// a CONNECT to those would turn the exit node into a proxy onto the VPS itself.
/// Private and link-local ranges are the operator's call, because a router-side
/// SOCKS5 client (podkop and friends) legitimately needs to reach LAN hosts
/// through the tunnel, while a shared exit node must not.
fn allowed_destination(address: IpAddr, allow_private: bool) -> bool {
    match address {
        IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && ip.octets() != [255, 255, 255, 255]
                && (allow_private || (!ip.is_link_local() && !ip.is_private()))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return allowed_destination(IpAddr::V4(mapped), allow_private);
            }
            let link_local = ip.segments()[0] & 0xffc0 == 0xfe80;
            let unique_local = ip.segments()[0] & 0xfe00 == 0xfc00;
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && (allow_private || !(link_local || unique_local))
        }
    }
}

async fn resolve_target(host: &str, port: u16, allow_private: bool) -> Result<SocketAddr> {
    let mut addresses = tokio::time::timeout(Duration::from_secs(10), lookup_host((host, port)))
        .await
        .map_err(|_| anyhow::anyhow!("DNS timeout"))??;
    addresses
        .find(|address| allowed_destination(address.ip(), allow_private))
        .ok_or_else(|| {
            anyhow::anyhow!("target is not a permitted address (allow_private={allow_private})")
        })
}

fn send_frame(app: &Arc<App>, session_id: u64, frame: Vec<u8>) -> Result<()> {
    protocol::command(
        app,
        protocol::ProtocolCommand::SendPlain {
            session_id,
            payload: frame,
        },
    )
}

fn enqueue_data(
    streams: &dashmap::DashMap<(u64, u64), mpsc::Sender<StreamInput>>,
    key: (u64, u64),
    data: &[u8],
) -> bool {
    let overflow = streams
        .get(&key)
        .is_some_and(|sender| sender.try_send(StreamInput::Data(data.to_vec())).is_err());
    if overflow {
        streams.remove(&key);
    }
    overflow
}

/// Range within the resend tail reachable from `offset`, if any.
fn resend_range(out_tail_len: u64, out_total: u64, offset: u64) -> Option<usize> {
    let oldest = out_total.saturating_sub(out_tail_len);
    if offset < oldest || offset >= out_total {
        return None;
    }
    let start = (offset - oldest) as usize;
    if start > out_tail_len as usize {
        return None;
    }
    Some(start)
}

pub async fn handle_frame(app: &Arc<App>, session_id: u64, payload: &[u8]) -> Result<()> {
    let frame = parse_frame(payload).ok_or_else(|| anyhow::anyhow!("invalid proxy frame"))?;
    let key = (session_id, frame.stream_id);
    match frame.kind {
        OPEN => {
            let session_count = app
                .proxy_streams
                .iter()
                .filter(|entry| entry.key().0 == session_id)
                .count();
            if app.proxy_streams.contains_key(&key) {
                // A retransmitted duplicate of an OPEN that already lives in
                // the table: acknowledging it again would send OPEN_ERR and
                // kill a healthy stream, so the duplicate is ignored.
                return Ok(());
            }
            if app.proxy_streams.len() >= MAX_GLOBAL_STREAMS || session_count >= MAX_SESSION_STREAMS
            {
                send_frame(
                    app,
                    session_id,
                    encode_frame(OPEN_ERR, frame.stream_id, &[1]),
                )?;
                return Ok(());
            }
            let target = match parse_target(frame.payload) {
                Ok(target) => target,
                Err(_) => {
                    send_frame(
                        app,
                        session_id,
                        encode_frame(OPEN_ERR, frame.stream_id, &[8]),
                    )?;
                    return Ok(());
                }
            };
            let (tx, rx) = mpsc::channel(64);
            app.proxy_streams.insert(key, tx);
            let app = app.clone();
            tokio::spawn(run_stream(app, session_id, frame.stream_id, target, rx));
        }
        DATA => {
            if frame.payload.len() > MAX_DATA + 8 {
                bail!("proxy data frame too large");
            }
            if enqueue_data(&app.proxy_streams, key, frame.payload) {
                send_frame(app, session_id, encode_frame(CLOSE, frame.stream_id, &[]))?;
            }
        }
        CLOSE => {
            if let Some((_, sender)) = app.proxy_streams.remove(&key) {
                let _ = sender.try_send(StreamInput::Close);
            }
        }
        RESEND => {
            let Ok(offset) = frame.payload[..].try_into() else {
                return Ok(());
            };
            let offset = u64::from_be_bytes(offset);
            if app
                .proxy_streams
                .get(&key)
                .is_some_and(|sender| sender.try_send(StreamInput::Resend(offset)).is_err())
            {
                app.proxy_streams.remove(&key);
            }
        }
        _ => return Ok(()),
    }
    Ok(())
}

async fn run_stream(
    app: Arc<App>,
    session_id: u64,
    stream_id: u64,
    target: (String, u16),
    mut input: mpsc::Receiver<StreamInput>,
) {
    let result = async {
        let address = resolve_target(&target.0, target.1, app.allow_private_destinations).await?;
        let mut stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(address))
            .await.map_err(|_| anyhow::anyhow!("connect timeout"))??;
        stream.set_nodelay(true)?;
        send_frame(&app, session_id, encode_frame(OPEN_OK, stream_id, &[]))?;
        let mut buffer = vec![0u8; MAX_DATA];
        let mut sent = StreamSequence::default();
        let mut received = crate::proxy_sequence::ReorderReceiver::default();
        let mut out_tail = Vec::<u8>::new();
        let mut hole_deadline = None;
        let mut last_activity = tokio::time::Instant::now();
        loop {
            if received.is_waiting() {
                if hole_deadline.is_none() {
                    hole_deadline = Some(tokio::time::Instant::now() + DATA_HOLE_PATIENCE);
                }
            } else {
                hole_deadline = None;
            }
            let quiet_due = last_activity + DATA_QUIET_TIMEOUT;
            let due = match hole_deadline {
                Some(hole_due) => quiet_due.min(hole_due),
                None => quiet_due,
            };
            let stall = async move { tokio::time::sleep_until(due).await };
            tokio::pin!(stall);
            tokio::select! {
                read = stream.read(&mut buffer) => match read? {
                    0 => break,
                    length => {
                        last_activity = tokio::time::Instant::now();
                        let plaintext = &buffer[..length];
                        out_tail.extend_from_slice(plaintext);
                        let overflow = out_tail.len().saturating_sub(RESEND_BUFFER_BYTES);
                        if overflow > 0 {
                            out_tail.drain(..overflow.min(out_tail.len()));
                        }
                        send_frame(&app, session_id, encode_frame(DATA, stream_id, &sent.encode(plaintext)?))?
                    }
                },
                command = input.recv() => match command {
                    Some(StreamInput::Data(data)) => {
                        last_activity = tokio::time::Instant::now();
                        if let Some(decoded) = received.push(&data)? {
                            stream.write_all(&decoded).await?;
                        }
                    }
                    Some(StreamInput::Resend(offset)) => {
                        let out_total = sent.offset();
                        let out_tail_len = out_tail.len() as u64;
                        let Some(start) = resend_range(out_tail_len, out_total, offset) else {
                            continue;
                        };
                        let mut cursor = offset;
                        let mut slice = &out_tail[start..];
                        let mut frames = 0usize;
                        while !slice.is_empty() && frames < RESEND_MAX_FRAMES {
                            let take = slice.len().min(MAX_DATA);
                            let chunk = &slice[..take];
                            send_frame(
                                &app,
                                session_id,
                                encode_frame(DATA, stream_id, &StreamSequence::encode_at(cursor, chunk)?),
                            )?;
                            cursor += take as u64;
                            slice = &slice[take..];
                            frames += 1;
                        }
                    }
                    Some(StreamInput::Close) | None => break,
                },
                _ = &mut stall => {
                    bail!("proxy stream stalled: no transport activity");
                }
            }
        }
        Result::<()>::Ok(())
    }.await;
    app.proxy_streams.remove(&(session_id, stream_id));
    if result.is_err() {
        let _ = send_frame(&app, session_id, encode_frame(OPEN_ERR, stream_id, &[5]));
    }
    let _ = send_frame(&app, session_id, encode_frame(CLOSE, stream_id, &[]));
}

pub fn close_session(app: &Arc<App>, session_id: u64) {
    let keys: Vec<_> = app
        .proxy_streams
        .iter()
        .filter_map(|entry| (entry.key().0 == session_id).then_some(*entry.key()))
        .collect();
    for key in keys {
        if let Some((_, sender)) = app.proxy_streams.remove(&key) {
            let _ = sender.try_send(StreamInput::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn full_stream_queue_closes_without_delivering_a_suffix_after_a_gap() {
        let streams = dashmap::DashMap::new();
        let (tx, mut rx) = mpsc::channel(1);
        streams.insert((1, 2), tx);
        assert!(!enqueue_data(&streams, (1, 2), b"prefix"));
        assert!(enqueue_data(&streams, (1, 2), b"lost"));
        assert!(!enqueue_data(&streams, (1, 2), b"suffix"));
        assert!(matches!(rx.recv().await, Some(StreamInput::Data(bytes)) if bytes == b"prefix"));
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn target_parser_accepts_domain_and_rejects_zero_port() {
        let mut payload = vec![3, 11];
        payload.extend_from_slice(b"example.com");
        payload.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(
            parse_target(&payload).unwrap(),
            ("example.com".to_owned(), 443)
        );
        let length = payload.len();
        payload[length - 2..].copy_from_slice(&0u16.to_be_bytes());
        assert!(parse_target(&payload).is_err());
    }

    #[test]
    fn private_and_metadata_destinations_are_blocked() {
        assert!(!allowed_destination("127.0.0.1".parse().unwrap(), false));
        assert!(!allowed_destination("10.0.0.1".parse().unwrap(), false));
        assert!(!allowed_destination(
            "169.254.169.254".parse().unwrap(),
            false
        ));
        assert!(!allowed_destination(
            "::ffff:127.0.0.1".parse().unwrap(),
            false
        ));
        assert!(allowed_destination("1.1.1.1".parse().unwrap(), false));
    }

    #[test]
    fn resend_range_covers_only_the_reachable_suffix() {
        let tail_len = 1024u64;
        let total = 100_000u64;
        // Tail holds [98976, 100000): offset inside it is reachable.
        assert_eq!(resend_range(tail_len, total, 99_000), Some(24));
        // The exact oldest byte of the tail.
        assert_eq!(resend_range(tail_len, total, 98_976), Some(0));
        // The exact end is not reachable (nothing new to send).
        assert_eq!(resend_range(tail_len, total, 100_000), None);
        // Too old: beyond the tail.
        assert_eq!(resend_range(tail_len, total, 98_975), None);
        // Requesting future data is ignored.
        assert_eq!(resend_range(tail_len, total, 100_001), None);
        // Degenerate: empty tail keeps nothing replayable.
        assert_eq!(resend_range(0, 0, 0), None);
    }

    #[test]
    fn private_targets_stay_blocked_by_default() {
        for blocked in [
            "192.168.11.1",
            "10.0.0.5",
            "172.16.4.4",
            "169.254.10.1",
            "127.0.0.1",
            "::1",
            "224.0.0.1",
            "255.255.255.255",
            "0.0.0.0",
            "fd00::1",
            "fe80::1",
        ] {
            let address: IpAddr = blocked.parse().unwrap();
            assert!(
                !allowed_destination(address, false),
                "{blocked} must not be reachable without the private-destination opt-in"
            );
        }
    }

    #[test]
    fn private_opt_in_unlocks_lan_but_never_the_server_itself() {
        for allowed in [
            "192.168.11.1",
            "10.0.0.5",
            "172.16.4.4",
            "169.254.10.1",
            "fd00::1",
        ] {
            let address: IpAddr = allowed.parse().unwrap();
            assert!(
                allowed_destination(address, true),
                "{allowed} must be reachable for a router-side SOCKS5 client"
            );
        }
        for still_blocked in [
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
        ] {
            let address: IpAddr = still_blocked.parse().unwrap();
            assert!(
                !allowed_destination(address, true),
                "{still_blocked} must stay blocked even with the opt-in"
            );
        }
    }

    #[test]
    fn public_targets_are_unaffected_by_the_opt_in() {
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            let address: IpAddr = public.parse().unwrap();
            assert!(allowed_destination(address, false));
            assert!(allowed_destination(address, true));
        }
    }
}
