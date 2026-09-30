// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use crate::{dispatcher::Dispatcher, packet::PacketPool};
use anyhow::{Context, Result, bail};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub const MAGIC: &[u8; 6] = b"CSQPX2";
const HEADER_LEN: usize = MAGIC.len() + 1 + 8;
const MAX_DATA: usize = 1_280;
use crate::proxy_sequence::StreamSequence;
/// Default concurrent CONNECT cap. Sized for a full-tunnel router use case
/// (podkop and friends) rather than a single browser; override per deployment
/// with `--socks5-max-streams`, 0 disables the cap.
pub const DEFAULT_MAX_STREAMS: usize = 256;
/// How long a CSQPX2 reorder hole may stay open before the stream is reset.
/// Long enough to absorb a burst reordering through the duplicated copies, short
/// enough to fail fast (and let the browser retry) when the leg is truly dead.
const DATA_HOLE_PATIENCE: Duration = Duration::from_secs(6);
/// Absolute quiet timeout: if the tunnel delivers no DATA frames for a stream
/// for this long, the leg has silently stopped and the stream is reset instead
/// of hanging until the browser gives up. Long enough that a genuinely idle
/// stream (WebSocket without traffic, SSE/long-poll, keep-alive) is left
/// alone, yet well under the browser's own ~2min idle timeout so the retry
/// still lands on a fresh carrier; longer than the hole patience because a
/// missing close/tear leaves no detectable hole at all.
const DATA_QUIET_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const OPEN: u8 = 1;
const OPEN_OK: u8 = 2;
const OPEN_ERR: u8 = 3;
const DATA: u8 = 4;
pub(crate) const CLOSE: u8 = 5;
const RESEND: u8 = 6;
/// How often the client asks the server to replay a missing suffix while a
/// reorder hole stays open. Sparser than the hole patience so at least one
/// retransmission attempt lands before the stream is reset.
const RESEND_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug)]
enum Inbound {
    Opened,
    Error(u8),
    Data(Vec<u8>),
    Closed,
}

struct Frame<'a> {
    kind: u8,
    stream_id: u64,
    payload: &'a [u8],
}

pub fn is_frame(payload: &[u8]) -> bool {
    payload.len() >= HEADER_LEN && payload.starts_with(MAGIC)
}

pub(crate) fn frame_route(payload: &[u8]) -> Option<(u8, u64)> {
    parse_frame(payload).map(|frame| (frame.kind, frame.stream_id))
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

fn deliver_inbound(streams: &mut HashMap<u64, mpsc::Sender<Inbound>>, id: u64, event: Inbound) {
    if let Some(sender) = streams.get(&id)
        && sender.try_send(event).is_err()
    {
        // Close after the queued prefix; never deliver bytes following the lost frame.
        streams.remove(&id);
    }
}

pub async fn start(
    bind: &str,
    max_streams: usize,
    dispatcher: Arc<Dispatcher>,
    pool: Arc<PacketPool>,
    cancel: CancellationToken,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let requested: SocketAddr = bind.parse().context("invalid SOCKS5 bind address")?;
    let listener = TcpListener::bind(requested)
        .await
        .context("SOCKS5 bind failed")?;
    let address = listener.local_addr()?;
    let streams = Arc::new(Mutex::new(HashMap::<u64, mpsc::Sender<Inbound>>::new()));
    let (frame_tx, mut frame_rx) = mpsc::channel::<Vec<u8>>(512);
    dispatcher.set_proxy_frame_sender(frame_tx)?;
    let inbound_streams = streams.clone();
    let (abort_tx, mut abort_rx) = mpsc::channel::<u64>(256);
    dispatcher.set_proxy_abort(abort_tx)?;
    let abort_streams = inbound_streams.clone();
    tokio::spawn(async move {
        while let Some(stream_id) = abort_rx.recv().await {
            let mut streams = abort_streams.lock().await;
            deliver_inbound(&mut streams, stream_id, Inbound::Closed);
        }
    });
    let inbound_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            let payload = tokio::select! {
                _ = inbound_cancel.cancelled() => return,
                payload = frame_rx.recv() => match payload { Some(payload) => payload, None => return },
            };
            let Some(frame) = parse_frame(&payload) else {
                continue;
            };
            let event = match frame.kind {
                OPEN_OK => Inbound::Opened,
                OPEN_ERR => Inbound::Error(frame.payload.first().copied().unwrap_or(1)),
                DATA if frame.payload.len() <= MAX_DATA + 8 => {
                    Inbound::Data(frame.payload.to_vec())
                }
                CLOSE => Inbound::Closed,
                _ => continue,
            };
            let mut streams = inbound_streams.lock().await;
            deliver_inbound(&mut streams, frame.stream_id, event);
        }
    });

    let task = tokio::spawn(async move {
        let ids = AtomicU64::new(1);
        loop {
            let accepted = tokio::select! {
                _ = cancel.cancelled() => return,
                accepted = listener.accept() => accepted,
            };
            let Ok((mut socket, _)) = accepted else {
                continue;
            };
            if max_streams > 0 && streams.lock().await.len() >= max_streams {
                // Never drop silently: callers that route whole subnets
                // through this proxy would otherwise see a bare connection
                // close and retry forever with no clue why.
                crate::log_error!(
                    "[SOCKS5] Достигнут лимит одновременных потоков ({max_streams}), соединение отклонено"
                );
                let _ = socket.shutdown().await;
                drop(socket);
                continue;
            }
            let id = ids.fetch_add(1, Ordering::Relaxed).max(1);
            let dispatcher = dispatcher.clone();
            let pool = pool.clone();
            let streams = streams.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    handle_client(socket, id, dispatcher, pool, streams, cancel).await
                    && !error.to_string().contains("closed")
                {
                    crate::log_error!("[SOCKS5] Поток #{id} закрыт: {error:#}");
                }
            });
        }
    });
    Ok((address, task))
}

async fn handle_client(
    mut socket: TcpStream,
    stream_id: u64,
    dispatcher: Arc<Dispatcher>,
    pool: Arc<PacketPool>,
    streams: Arc<Mutex<HashMap<u64, mpsc::Sender<Inbound>>>>,
    cancel: CancellationToken,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let target = read_handshake(&mut socket).await?;
    let (tx, mut rx) = mpsc::channel(64);
    streams.lock().await.insert(stream_id, tx);
    let started = tokio::time::Instant::now();
    let stats_arg = dispatcher.stats();
    let ibound0 = stats_arg.inbound_datagrams.load(Ordering::Relaxed);
    let obound0 = stats_arg.outbound_datagrams.load(Ordering::Relaxed);
    let (mut down_frames, mut down_bytes) = (0u64, 0u64);
    let mut resends = 0u64;
    let result = async {
        let open_frame = encode_frame(OPEN, stream_id, &target);
        let spread_base = (stream_id & 0xffff) as usize;
        let mut opened = None;
        let mut attempt = 0u64;
        let open_deadline = tokio::time::Instant::now() + Duration::from_secs(9);
        while tokio::time::Instant::now() < open_deadline {
            if let Err(error) = dispatcher
                .send_proxy_open(&pool, &open_frame, spread_base.wrapping_add(attempt as usize))
                .await
            {
                // No ready carrier: the listener must still answer. A bare
                // close is indistinguishable from a network fault for the
                // caller, which would then retry a proxy that is up.
                crate::log_error!("[SOCKS5] Нет готового канала для CONNECT: {error:#}");
                let _ = write_reply(&mut socket, 1).await;
                bail!("no carrier available for SOCKS5 CONNECT");
            }
            match tokio::time::timeout(Duration::from_millis(2000), rx.recv()).await {
                Ok(Some(Inbound::Opened)) => {
                    opened = Some(Inbound::Opened);
                    break;
                }
                Ok(Some(Inbound::Error(code))) => {
                    opened = Some(Inbound::Error(code));
                    break;
                }
                _ => attempt += 1,
            }
        }
        match opened {
            Some(Inbound::Opened) => write_reply(&mut socket, 0).await?,
            Some(Inbound::Error(code)) => {
                write_reply(&mut socket, code).await?;
                bail!("remote SOCKS5 connect failed");
            }
            _ => {
                write_reply(&mut socket, 4).await?;
                bail!("remote SOCKS5 connect timed out");
            }
        }
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = vec![0u8; MAX_DATA];
        let mut sent = StreamSequence::default();
        let mut received = crate::proxy_sequence::ReorderReceiver::default();
        let mut hole_deadline = None;
        let mut last_down_activity = tokio::time::Instant::now();
        let mut last_resend = None;
        loop {
            if received.is_waiting() {
                if hole_deadline.is_none() {
                    hole_deadline = Some(tokio::time::Instant::now() + DATA_HOLE_PATIENCE);
                }
                // Ask the server to replay the missing suffix. Refresh the hole
                // deadline on each successfully queued request so a healthy but
                // lossy carrier gets enough retransmission attempts, while a
                // truly dead leg still fails fast once requests stop helping.
                let now = tokio::time::Instant::now();
                if last_resend
                    .is_none_or(|last| now.duration_since(last) >= RESEND_INTERVAL)
                    && let Some(offset) = received.missing_offset()
                {
                    let mut payload = Vec::with_capacity(8);
                    payload.extend_from_slice(&offset.to_be_bytes());
                    let queued = dispatcher
                        .send_proxy_frame(&pool, &encode_frame(RESEND, stream_id, &payload))
                        .await;
                    if queued.is_ok() {
                        resends += 1;
                        last_resend = Some(now);
                        hole_deadline = Some(now + DATA_HOLE_PATIENCE);
                    }
                }
            } else {
                hole_deadline = None;
            }
            let quiet_due = last_down_activity + DATA_QUIET_TIMEOUT;
            let due = match hole_deadline {
                Some(hole_due) => quiet_due.min(hole_due),
                None => quiet_due,
            };
            let stall = async move { tokio::time::sleep_until(due).await };
            tokio::pin!(stall);
            tokio::select! {
                _ = cancel.cancelled() => break,
                                read = reader.read(&mut buffer) => match read? {
                    0 => break,
                    length => {
                        dispatcher.stats().total_bytes_up.fetch_add(length as i64, Ordering::Relaxed);
                        dispatcher
                            .send_proxy_frame(
                                &pool,
                                &encode_frame(DATA, stream_id, &sent.encode(&buffer[..length])?),
                            )
                            .await?
                    }
                },
                inbound = rx.recv() => match inbound {
                    Some(Inbound::Data(data)) => {
                        last_down_activity = tokio::time::Instant::now();
                        if let Some(decoded) = received.push(&data)? {
                            down_frames += 1;
                            down_bytes += decoded.len() as u64;
                            dispatcher.stats().total_bytes_down.fetch_add(decoded.len() as i64, Ordering::Relaxed);
                            writer.write_all(&decoded).await?;
                        }
                    }
                    Some(Inbound::Closed | Inbound::Error(_)) | None => break,
                    Some(Inbound::Opened) => {}
                },
                _ = &mut stall => {
                    // Either a reorder hole stayed open without progress or the
                    // tunnel went completely quiet for this stream. The leg is
                    // effectively dead: fail fast so the browser can retry on
                    // a healthy carrier instead of hanging on ERR/HTTP timeout.
                    bail!("proxy stream stalled: no downlink progress");
                }
            }
        }
        Result::<()>::Ok(())
    }.await;
    eprintln!(
        "[SXP] stream {stream_id} ended: down_frames={down_frames} down_bytes={down_bytes} resends={resends} ok={:?} dt={}ms ibound_delta={} obound_delta={}",
        result.is_ok(),
        started.elapsed().as_millis(),
        stats_arg
            .inbound_datagrams
            .load(Ordering::Relaxed)
            .saturating_sub(ibound0),
        stats_arg
            .outbound_datagrams
            .load(Ordering::Relaxed)
            .saturating_sub(obound0),
    );
    streams.lock().await.remove(&stream_id);
    let _ = dispatcher
        .send_proxy_frame(&pool, &encode_frame(CLOSE, stream_id, &[]))
        .await;
    result
}

async fn read_handshake(socket: &mut TcpStream) -> Result<Vec<u8>> {
    let mut greeting = [0u8; 2];
    socket.read_exact(&mut greeting).await?;
    if greeting[0] != 5 || greeting[1] == 0 {
        bail!("invalid SOCKS5 greeting");
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 0xff]).await?;
        bail!("SOCKS5 client does not support no-auth mode");
    }
    socket.write_all(&[5, 0]).await?;
    let mut request = [0u8; 4];
    socket.read_exact(&mut request).await?;
    if request[0] != 5 || request[1] != 1 || request[2] != 0 {
        write_reply(socket, 7).await?;
        bail!("only SOCKS5 CONNECT is supported");
    }
    let mut target = vec![request[3]];
    match request[3] {
        1 => {
            let mut rest = [0u8; 6];
            socket.read_exact(&mut rest).await?;
            target.extend_from_slice(&rest);
        }
        4 => {
            let mut rest = [0u8; 18];
            socket.read_exact(&mut rest).await?;
            target.extend_from_slice(&rest);
        }
        3 => {
            let length = socket.read_u8().await? as usize;
            if length == 0 {
                bail!("empty SOCKS5 domain");
            }
            target.push(length as u8);
            let mut rest = vec![0u8; length + 2];
            socket.read_exact(&mut rest).await?;
            target.extend_from_slice(&rest);
        }
        _ => {
            write_reply(socket, 8).await?;
            bail!("unsupported SOCKS5 address type");
        }
    }
    Ok(target)
}

async fn write_reply(socket: &mut TcpStream, code: u8) -> Result<()> {
    socket.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dispatcher::{WorkerChannels, packet_channel},
        stats::Stats,
    };

    #[test]
    fn proxy_frame_round_trip_is_strict() {
        let encoded = encode_frame(DATA, 42, b"payload");
        let frame = parse_frame(&encoded).unwrap();
        assert_eq!(frame.kind, DATA);
        assert_eq!(frame.stream_id, 42);
        assert_eq!(frame.payload, b"payload");
        assert!(!is_frame(b"ordinary IP packet"));
    }

    #[test]
    fn resend_frame_round_trip_and_routing() {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&123_456u64.to_be_bytes());
        let encoded = encode_frame(RESEND, 7, &payload);
        let frame = parse_frame(&encoded).unwrap();
        assert_eq!(frame.kind, RESEND);
        assert_eq!(frame.stream_id, 7);
        assert_eq!(
            u64::from_be_bytes(frame.payload.try_into().unwrap()),
            123_456
        );
        // RESEND is routed through the pinned carrier like DATA, not like OPEN.
        let (kind, stream_id) = frame_route(&encoded).unwrap();
        assert_eq!(kind, RESEND);
        assert_eq!(stream_id, 7);
    }

    #[tokio::test]
    async fn inbound_overflow_closes_only_the_affected_stream() {
        let mut streams = HashMap::new();
        let (tx, mut rx) = mpsc::channel(1);
        let (other, _other_rx) = mpsc::channel(1);
        streams.insert(1, tx);
        streams.insert(2, other);
        deliver_inbound(&mut streams, 1, Inbound::Data(b"prefix".to_vec()));
        deliver_inbound(&mut streams, 1, Inbound::Data(b"lost".to_vec()));
        deliver_inbound(&mut streams, 1, Inbound::Data(b"suffix".to_vec()));
        assert!(streams.contains_key(&2));
        assert!(!streams.contains_key(&1));
        assert!(matches!(rx.recv().await, Some(Inbound::Data(bytes)) if bytes == b"prefix"));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn proxy_queue_overflow_never_evicts_earlier_bytes() {
        let pool = PacketPool::new(32);
        let cancel = CancellationToken::new();
        let (dispatcher, _) = Dispatcher::start(
            "127.0.0.1:0",
            None,
            pool.clone(),
            Arc::new(Stats::default()),
            cancel.clone(),
        )
        .await
        .unwrap();
        let (latency, _latency_rx) = packet_channel(1, true);
        let (priority, priority_rx) = packet_channel(1, true);
        let (bulk, _bulk_rx) = packet_channel(1, true);
        dispatcher.register(WorkerChannels {
            id: 1,
            incarnation_id: 1,
            turn_path: Arc::from("test"),
            latency,
            priority,
            bulk,
        });
        dispatcher
            .send_proxy_frame(&pool, &encode_frame(OPEN, 1, b"target"))
            .await
            .unwrap();
        // The worker's single-slot priority queue is now full: the next frame
        // must back-pressure (wait for a drain) rather than evict the pending
        // OPEN or tear down the whole tunnel.
        let worker_flow = {
            let dispatcher = dispatcher.clone();
            let pool = pool.clone();
            tokio::spawn(async move {
                dispatcher
                    .send_proxy_frame(&pool, &encode_frame(DATA, 1, b"later bytes"))
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !worker_flow.is_finished(),
            "uplink overflow must back-pressure, not evict earlier bytes"
        );
        let open = priority_rx.try_recv().unwrap();
        assert_eq!(parse_frame(open.as_slice()).unwrap().kind, OPEN);
        let _ = worker_flow.await.unwrap();
        let data = priority_rx.try_recv().unwrap();
        assert_eq!(parse_frame(data.as_slice()).unwrap().kind, DATA);
        assert!(!cancel.is_cancelled());

        let (tx, mut rx) = mpsc::channel(1);
        dispatcher.set_proxy_frame_sender(tx).unwrap();
        let push_frame = |dispatcher: Arc<Dispatcher>, pool: Arc<PacketPool>| {
            let bytes = encode_frame(DATA, 1, b"response");
            let mut packet = pool.acquire();
            packet.set_read_len(bytes.len()).unwrap();
            packet.as_mut_slice().copy_from_slice(&bytes);
            async move { dispatcher.return_packet(packet).await }
        };
        // The single-slot frame pipe is now full: a second frame must
        // back-pressure (wait for the first to drain) rather than evict it or
        // tear down the whole tunnel.
        push_frame(dispatcher.clone(), pool.clone()).await;
        let second = tokio::spawn(push_frame(dispatcher.clone(), pool.clone()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !second.is_finished(),
            "overflow must back-pressure instead of evicting the first frame"
        );
        let first = rx.try_recv().unwrap();
        assert_eq!(parse_frame(&first).unwrap().kind, DATA);
        assert!(!cancel.is_cancelled());
        second.await.unwrap();
        assert_eq!(parse_frame(&rx.try_recv().unwrap()).unwrap().kind, DATA);
        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn local_socks5_connect_and_data_use_csqtt_frames() {
        let pool = PacketPool::new(32);
        let cancel = CancellationToken::new();
        let (dispatcher, _) = Dispatcher::start(
            "127.0.0.1:0",
            None,
            pool.clone(),
            Arc::new(Stats::default()),
            cancel.clone(),
        )
        .await
        .unwrap();
        let (latency, _latency_rx) = packet_channel(8, true);
        let (priority, priority_rx) = packet_channel(16, true);
        let (bulk, _bulk_rx) = packet_channel(8, true);
        dispatcher.register(WorkerChannels {
            id: 1,
            incarnation_id: 1,
            turn_path: Arc::from("test"),
            latency,
            priority,
            bulk,
        });
        let (address, task) = start(
            "127.0.0.1:0",
            DEFAULT_MAX_STREAMS,
            dispatcher.clone(),
            pool.clone(),
            cancel.clone(),
        )
        .await
        .unwrap();
        let mut client = TcpStream::connect(address).await.unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0u8; 2];
        client.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);
        client.write_all(&[5, 1, 0, 3, 11]).await.unwrap();
        client.write_all(b"example.com").await.unwrap();
        client.write_all(&443u16.to_be_bytes()).await.unwrap();

        let open_packet = priority_rx.recv(&cancel).await.unwrap();
        let open = parse_frame(open_packet.as_slice()).unwrap();
        assert_eq!(open.kind, OPEN);
        let stream_id = open.stream_id;
        assert_eq!(open.payload[0], 3);

        let (latency, _latency_rx2) = packet_channel(8, true);
        let (priority, priority_rx2) = packet_channel(16, true);
        let (bulk, _bulk_rx2) = packet_channel(8, true);
        dispatcher.register(WorkerChannels {
            id: 0,
            incarnation_id: 2,
            turn_path: Arc::from("test"),
            latency,
            priority,
            bulk,
        });

        let opened = encode_frame(OPEN_OK, stream_id, &[]);
        let mut packet = pool.acquire();
        packet.set_read_len(opened.len()).unwrap();
        packet.as_mut_slice().copy_from_slice(&opened);
        dispatcher.return_packet(packet).await;
        let mut reply = [0u8; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);

        client.write_all(b"hello").await.unwrap();
        let data_packet = priority_rx.recv(&cancel).await.unwrap();
        let data = parse_frame(data_packet.as_slice()).unwrap();
        assert_eq!(data.kind, DATA);
        assert_eq!(&data.payload[8..], b"hello");
        assert!(priority_rx2.try_recv().is_none());

        let response = encode_frame(
            DATA,
            stream_id,
            &StreamSequence::default().encode(b"world").unwrap(),
        );
        let mut packet = pool.acquire();
        packet.set_read_len(response.len()).unwrap();
        packet.as_mut_slice().copy_from_slice(&response);
        dispatcher.return_packet(packet).await;
        let mut body = [0u8; 5];
        client.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"world");

        cancel.cancel();
        dispatcher.shutdown().await;
        let _ = task.await;
    }
}
