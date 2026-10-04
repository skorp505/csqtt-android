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
        let ids = Arc::new(AtomicU64::new(1));
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
            let ids = ids.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    handle_client(socket, ids, dispatcher, pool, streams, cancel).await
                    && !error.to_string().contains("closed")
                {
                    crate::log_error!("[SOCKS5] Поток #{id} закрыт: {error:#}");
                }
            });
        }
    });
    Ok((address, task))
}

/// One OPEN attempt gets a single carrier and a short budget. A carrier that
/// never answers costs `OPEN_ATTEMPT_TIMEOUT`, and the retry moves on instead
/// of stalling the caller's CONNECT for the whole budget.
const OPEN_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(1500);
/// OPEN_OK only proves the server reached the target; the first DATA frame is
/// what proves this carrier's downlink actually carries payload. A relay that
/// silently blackholes the stream is only visible here.
const FIRST_DATA_TIMEOUT: Duration = Duration::from_millis(2500);
const OPEN_ATTEMPTS: usize = 4;
/// Wall-clock ceiling for all attempts combined, so a caller never waits longer
/// than the old single-carrier deadline.
const OPEN_BUDGET: Duration = Duration::from_secs(9);

/// Whether parked caller bytes may be sent again on a replacement carrier.
///
/// Nothing parked means the caller has not spoken yet, which is always safe to
/// repeat. Otherwise only a request that clearly starts with an idempotent HTTP
/// method qualifies: a first copy may already have reached the target, and a
/// silent duplicate POST is worse than a failed connection the caller retries.
/// Anything unrecognised, TLS included, counts as unsafe to repeat.
fn replayable(parked: &[u8]) -> bool {
    const IDEMPOTENT: [&[u8]; 4] = [b"GET ", b"HEAD ", b"OPTIONS ", b"TRACE "];
    parked.is_empty() || IDEMPOTENT.iter().any(|method| parked.starts_with(method))
}

/// Upper bound on caller bytes parked while a tunnel generation is still
/// unproven. A request that fits here can be replayed onto the next carrier;
/// anything larger means the leg is hopeless rather than slow.
const UNPROVEN_UPLINK_CAP: usize = 16 * 1024;

/// Negotiate one tunnel generation: OPEN on a single carrier, wait for the
/// server to confirm the remote TCP connect.
///
/// Retries land on a different carrier each time, so a blackholed leg costs
/// `OPEN_ATTEMPT_TIMEOUT` instead of the whole budget. This only proves the
/// server reached the target; whether this carrier's downlink carries payload
/// is decided later by the first DATA frame.
async fn open_tunnel(
    target: &[u8],
    dispatcher: &Dispatcher,
    pool: &Arc<PacketPool>,
    streams: &Arc<Mutex<HashMap<u64, mpsc::Sender<Inbound>>>>,
    ids: &AtomicU64,
) -> Result<(u64, mpsc::Receiver<Inbound>), u8> {
    let budget_deadline = tokio::time::Instant::now() + OPEN_BUDGET;
    let mut attempt = 0usize;
    let mut last_error = 4u8;
    loop {
        if attempt >= OPEN_ATTEMPTS || tokio::time::Instant::now() >= budget_deadline {
            return Err(last_error);
        }
        let id = ids.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, mut rx) = mpsc::channel(64);
        streams.lock().await.insert(id, tx);
        let frame = encode_frame(OPEN, id, target);
        // Ids increase monotonically, so consecutive connections and successive
        // attempts of one connection land on different carriers.
        let worker_hint = (id as usize).wrapping_add(attempt);
        if dispatcher
            .send_proxy_open(pool, &frame, worker_hint)
            .await
            .is_ok()
        {
            let deadline =
                (tokio::time::Instant::now() + OPEN_ATTEMPT_TIMEOUT).min(budget_deadline);
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(Inbound::Opened)) => return Ok((id, rx)),
                Ok(Some(Inbound::Error(code))) => {
                    last_error = if code == 0 { 1 } else { code };
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        // This carrier never confirmed the connect: drop the generation and
        // keep the caller's connection alive for the next one.
        streams.lock().await.remove(&id);
        let _ = dispatcher
            .send_proxy_frame(pool, &encode_frame(CLOSE, id, &[]))
            .await;
        attempt += 1;
    }
}

async fn handle_client(
    mut socket: TcpStream,
    ids: Arc<AtomicU64>,
    dispatcher: Arc<Dispatcher>,
    pool: Arc<PacketPool>,
    streams: Arc<Mutex<HashMap<u64, mpsc::Sender<Inbound>>>>,
    cancel: CancellationToken,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let target = read_handshake(&mut socket).await?;
    let started = tokio::time::Instant::now();
    let stats_arg = dispatcher.stats();
    let ibound0 = stats_arg.inbound_datagrams.load(Ordering::Relaxed);
    let obound0 = stats_arg.outbound_datagrams.load(Ordering::Relaxed);
    let (mut down_frames, mut down_bytes) = (0u64, 0u64);
    let (mut resends, mut generations) = (0u64, 0u64);
    let mut stream_id = 0u64;
    let result = async {
        let (opened_id, mut rx) = match open_tunnel(&target, &dispatcher, &pool, &streams, &ids)
            .await
        {
            Ok(opened) => opened,
            Err(code) => {
                let _ = write_reply(&mut socket, code).await;
                bail!("no carrier confirmed SOCKS5 CONNECT");
            }
        };
        stream_id = opened_id;
        generations += 1;
        // The remote connect succeeded, so the caller may send. Its first bytes
        // stay parked until this carrier proves it can also deliver, which is
        // what makes a later switch to another carrier replayable.
        write_reply(&mut socket, 0).await?;
        let (mut reader, mut writer) = socket.into_split();
        let mut buffer = vec![0u8; MAX_DATA];
        let mut sent = StreamSequence::default();
        let mut received = crate::proxy_sequence::ReorderReceiver::default();
        let mut unproven_up = Vec::new();
        let mut proven = false;
        let mut proof_deadline = tokio::time::Instant::now() + FIRST_DATA_TIMEOUT;
        let mut hole_deadline = None;
        let mut last_down_activity = tokio::time::Instant::now();
        let mut last_resend = None;
        let mut switch = false;
        loop {
            if switch {
                // Move to a fresh carrier and replay the parked request. Done
                // here rather than inside the select! body so the borrow on
                // `rx` from the previous iteration has already ended.
                switch = false;
                if generations >= OPEN_ATTEMPTS as u64 {
                    bail!("no carrier delivered payload for SOCKS5 stream");
                }
                crate::log_error!(
                    "[SOCKS5] Поток #{stream_id} не получил данных, переключаюсь на другой канал"
                );
                streams.lock().await.remove(&stream_id);
                let _ = dispatcher
                    .send_proxy_frame(&pool, &encode_frame(CLOSE, stream_id, &[]))
                    .await;
                let (next_id, next_rx) =
                    match open_tunnel(&target, &dispatcher, &pool, &streams, &ids).await
                    {
                    Ok(next) => next,
                    Err(_) => bail!("no carrier left for SOCKS5 stream"),
                };
                generations += 1;
                stream_id = next_id;
                rx = next_rx;
                sent = StreamSequence::default();
                received = crate::proxy_sequence::ReorderReceiver::default();
                hole_deadline = None;
                last_resend = None;
                last_down_activity = tokio::time::Instant::now();
                proof_deadline = tokio::time::Instant::now() + FIRST_DATA_TIMEOUT;
                // Replay the request that produced no answer on the dead
                // carrier, so the new one has something to respond to. A
                // non-idempotent request is never resent: the first copy may
                // already have reached the target, and a silent duplicate POST
                // is worse than a failed connection the caller can retry.
                if !replayable(&unproven_up) {
                    bail!("carrier died before answering a non-idempotent request");
                }
                let mut replay = std::mem::take(&mut unproven_up);
                while !replay.is_empty() {
                    let take = replay.len().min(MAX_DATA);
                    let chunk = replay.drain(..take).collect::<Vec<u8>>();
                    dispatcher
                        .send_proxy_frame(
                            &pool,
                            &encode_frame(DATA, stream_id, &sent.encode(&chunk)?),
                        )
                        .await?;
                }
                continue;
            }
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
            let quiet_due = if proven {
                last_down_activity + DATA_QUIET_TIMEOUT
            } else {
                // Nothing has ever come back on this carrier. Give it a short,
                // bounded grace period, then move to another one instead of
                // holding the caller's connection open for a minute.
                proof_deadline
            };
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
                            .await?;
                        if !proven {
                            // Keep a copy so a switch to another carrier can
                            // replay the request that produced no answer. The
                            // request must go out immediately: waiting for the
                            // downlink to prove itself first would deadlock any
                            // protocol where the caller speaks first.
                            unproven_up.extend_from_slice(&buffer[..length]);
                            if unproven_up.len() > UNPROVEN_UPLINK_CAP {
                                bail!("caller sent {UNPROVEN_UPLINK_CAP}+ bytes with no downlink");
                            }
                        }
                    }
                },
                inbound = rx.recv() => match inbound {
                    Some(Inbound::Data(data)) => {
                        last_down_activity = tokio::time::Instant::now();
                        if !proven {
                            // This carrier carries payload: the parked copy has
                            // served its purpose and must never be replayed.
                            proven = true;
                            unproven_up.clear();
                            unproven_up.shrink_to_fit();
                            hole_deadline = None;
                            last_resend = None;
                        }
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
                    // Before the first payload arrives, a dead carrier is not
                    // fatal: another one may still work, and the caller has not
                    // seen anything yet, so the switch stays invisible.
                    if !proven {
                        switch = true;
                        continue;
                    }
                    bail!("proxy stream stalled: no downlink progress");
                }
            }
        }
        Result::<()>::Ok(())
    }.await;
    eprintln!(
        "[SXP] stream {stream_id} ended: down_frames={down_frames} down_bytes={down_bytes} resends={resends} generations={generations} ok={:?} dt={}ms ibound_delta={} obound_delta={}",
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
        dispatcher::{PacketReceiver, WorkerChannels, packet_channel},
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

    #[test]
    fn only_idempotent_requests_may_be_replayed_on_another_carrier() {
        assert!(replayable(b""));
        assert!(replayable(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n"));
        assert!(replayable(b"HEAD /x HTTP/1.1\r\n\r\n"));
        assert!(replayable(b"OPTIONS * HTTP/1.1\r\n\r\n"));
        // A request that may already have reached the target must not be
        // repeated behind the caller's back.
        assert!(!replayable(b"POST /pay HTTP/1.1\r\n\r\n"));
        assert!(!replayable(b"PUT /x HTTP/1.1\r\n\r\n"));
        assert!(!replayable(b"PATCH /x HTTP/1.1\r\n\r\n"));
        assert!(!replayable(b"DELETE /x HTTP/1.1\r\n\r\n"));
        // Unknown protocol: refuse to guess.
        assert!(!replayable(b"\x16\x03\x01\x02\x00"));
        assert!(!replayable(b"garbage without a request line"));
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

    /// Take the next frame the proxy stack emits on either carrier. Carrier
    /// selection is an internal detail, so tests assert on the frame sequence
    /// rather than on which worker happened to win the hint.
    async fn next_frame(
        first: &PacketReceiver,
        second: &PacketReceiver,
        cancel: &CancellationToken,
    ) -> (u8, u64, Vec<u8>) {
        let packet = tokio::select! {
            biased;
            packet = first.recv(cancel) => packet,
            packet = second.recv(cancel) => packet,
        }
        .expect("carrier channel closed");
        let frame = parse_frame(packet.as_slice()).unwrap();
        (frame.kind, frame.stream_id, frame.payload.to_vec())
    }

    #[tokio::test]
    async fn dead_first_carrier_is_replaced_and_the_request_is_replayed() {
        let pool = PacketPool::new(64);
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
        client.write_all(&[5, 1, 0, 3, 11]).await.unwrap();
        client.write_all(b"example.com").await.unwrap();
        client.write_all(&443u16.to_be_bytes()).await.unwrap();

        let (kind, dead_stream, payload) = next_frame(&priority_rx, &priority_rx2, &cancel).await;
        assert_eq!(kind, OPEN);
        assert_eq!(payload[0], 3);
        assert_eq!(&payload[2..2 + 11], b"example.com");

        // The server confirms the connect, so the caller is told the tunnel is
        // good and sends its request.
        let opened = encode_frame(OPEN_OK, dead_stream, &[]);
        let mut packet = pool.acquire();
        packet.set_read_len(opened.len()).unwrap();
        packet.as_mut_slice().copy_from_slice(&opened);
        dispatcher.return_packet(packet).await;
        let mut reply = [0u8; 10];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0);

        client.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let (kind, stream, payload) = next_frame(&priority_rx, &priority_rx2, &cancel).await;
        assert_eq!(kind, DATA);
        assert_eq!(stream, dead_stream);
        assert_eq!(&payload[8..], b"GET / HTTP/1.1\r\n");

        // This carrier confirms the connect but never delivers the answer. The
        // caller must not see a failure: the stream is retired and a fresh one
        // carries the same request.
        // The two frames race across carriers, so assert on the set: the dead
        // stream is retired and a replacement one is opened.
        let mut seen = Vec::new();
        for _ in 0..2 {
            seen.push(next_frame(&priority_rx, &priority_rx2, &cancel).await);
        }
        assert!(
            seen.contains(&(CLOSE, dead_stream, Vec::new())),
            "dead stream must be retired: {seen:?}"
        );
        let live_stream = seen
            .iter()
            .find(|frame| frame.0 == OPEN)
            .map(|frame| frame.1)
            .expect("replacement stream must be opened");
        assert_ne!(live_stream, dead_stream);

        let opened = encode_frame(OPEN_OK, live_stream, &[]);
        let mut packet = pool.acquire();
        packet.set_read_len(opened.len()).unwrap();
        packet.as_mut_slice().copy_from_slice(&opened);
        dispatcher.return_packet(packet).await;
        let (kind, stream, payload) = next_frame(&priority_rx, &priority_rx2, &cancel).await;
        assert_eq!(kind, DATA);
        assert_eq!(stream, live_stream);
        assert_eq!(&payload[8..], b"GET / HTTP/1.1\r\n");

        let response = encode_frame(
            DATA,
            live_stream,
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
