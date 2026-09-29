//! CSQPX2 integrity checks. A gap/reorder terminates the TCP stream; a
//! retransmitted duplicate copy of already-consumed bytes is dropped without
//! advancing the cursor. Separate cursors are used for the two directions.
//!
//! The strict [`StreamSequence`] is kept for the writer side and for tests.
//! Receivers must use [`ReorderReceiver`]: a single reordered or lost chunk
//! must not kill a healthy stream. Out-of-order chunks are buffered until the
//! hole fills (duplicated copies race to fill it), and only an overflow of
//! the reorder window or a caller-side patience timeout tears the stream.
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct StreamSequence {
    offset: u64,
}

// This module is compiled into both the client and the server, which use
// different halves of the API (the server replays suffixes, the client asks for
// them), so each binary sees the other's helpers as unused.
#[allow(dead_code)]
impl StreamSequence {
    pub fn encode(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let result = Self::encode_at(self.offset, bytes)?;
        self.advance(bytes.len())?;
        Ok(result)
    }

    /// Build a chunk with an explicit absolute offset, without advancing the
    /// cursor. Used by the server to replay a suffix from the stream tail in
    /// response to a client RESEND request.
    pub fn encode_at(offset: u64, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(8 + bytes.len());
        result.extend_from_slice(&offset.to_be_bytes());
        result.extend_from_slice(bytes);
        Ok(result)
    }

    /// Current absolute write offset (the byte offset of the next chunk).
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn decode<'a>(&mut self, payload: &'a [u8]) -> Result<&'a [u8]> {
        let Some(header) = payload.get(..8) else {
            bail!("missing proxy stream offset")
        };
        let offset = u64::from_be_bytes(header.try_into()?);
        if offset < self.offset {
            // Retransmitted copy of bytes already consumed: writing it again
            // would corrupt the byte stream, so drop the duplicate.
            return Ok(&[]);
        }
        if offset > self.offset {
            bail!(
                "proxy stream gap or reorder: expected {}, received {offset}",
                self.offset
            );
        }
        let bytes = &payload[8..];
        self.advance(bytes.len())?;
        Ok(bytes)
    }

    fn advance(&mut self, length: usize) -> Result<()> {
        self.offset = self
            .offset
            .checked_add(length as u64)
            .ok_or_else(|| anyhow::anyhow!("proxy stream offset overflow"))?;
        Ok(())
    }
}

/// Default bound on how many out-of-order bytes a receiver will buffer while
/// waiting for a missing chunk. Duplicated copies normally fill the hole
/// within a few frames, so the window only has to absorb reordering bursts.
const MAX_BUFFERED: usize = 128 * 1024;

/// Loss-and-reorder tolerant receiver for the CSQPX2 byte stream.
///
/// Chunks are identified by their absolute byte offset. A chunk exactly at the
/// expected offset is consumed immediately and flushes any buffered run behind
/// it; chunks ahead are parked in the reorder buffer; chunks behind are stale
/// duplicates and are dropped. The caller is expected to give up (reset the
/// stream) if [`Self::is_waiting`] stays true for too long.
#[derive(Default)]
pub struct ReorderReceiver {
    expected: u64,
    pending: BTreeMap<u64, Vec<u8>>,
    buffered_bytes: usize,
}

#[allow(dead_code)]
impl ReorderReceiver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the contiguous bytes made ready by this chunk, if any.
    ///
    /// `Ok(None)` means "nothing to forward yet" (a buffered gap filler or a
    /// stale duplicate). `Err` means the stream is beyond recovery (malformed
    /// frame or the reorder window overflowed).
    pub fn push(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        let Some(header) = payload.get(..8) else {
            bail!("missing proxy stream offset")
        };
        let offset = u64::from_be_bytes(header.try_into()?);
        if offset < self.expected {
            return Ok(None);
        }
        let chunk = payload[8..].to_vec();
        if offset == self.expected {
            let mut output = chunk;
            self.expected += output.len() as u64;
            while let Some(next) = self.pending.remove(&self.expected) {
                self.expected += next.len() as u64;
                self.buffered_bytes -= next.len();
                output.extend_from_slice(&next);
            }
            return Ok(Some(output));
        }
        if self.pending.contains_key(&offset) {
            return Ok(None);
        }
        self.buffered_bytes += chunk.len();
        if self.buffered_bytes > MAX_BUFFERED {
            bail!("proxy stream reorder window overflow");
        }
        self.pending.insert(offset, chunk);
        Ok(None)
    }

    /// Whether a hole is currently open and the stream is stalled waiting for
    /// a missing chunk. Callers use this to bound the patience window before
    /// resetting the stream.
    pub fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Receiver-side diagnostics: how many bytes are currently parked.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered_bytes
    }

    /// The absolute byte offset the receiver is waiting for while a hole is
    /// open, if any. Used to request a retransmission of the missing suffix.
    pub fn missing_offset(&self) -> Option<u64> {
        self.is_waiting().then_some(self.expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_bytes_round_trip() {
        let (mut send, mut receive) = (StreamSequence::default(), ReorderReceiver::new());
        for bytes in [b"abc".as_slice(), b"defgh", b"i"] {
            let frame = send.encode(bytes).unwrap();
            assert_eq!(receive.push(&frame).unwrap().unwrap(), bytes);
            assert!(!receive.is_waiting());
        }
    }

    #[test]
    fn out_of_order_is_buffered_then_flushed() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        let b = send.encode(b"def").unwrap();
        let c = send.encode(b"ghi").unwrap();
        let mut receiver = ReorderReceiver::new();
        // c (offset 6) arrives before a/b: buffered, no fatal error.
        assert_eq!(receiver.push(&c).unwrap(), None);
        assert!(receiver.is_waiting());
        assert_eq!(receiver.buffered_bytes(), 3);
        // b (offset 3) still leaves the hole at 0 open.
        assert_eq!(receiver.push(&b).unwrap(), None);
        assert!(receiver.is_waiting());
        assert_eq!(receiver.buffered_bytes(), 6);
        // a (offset 0) fills the hole and flushes a+b+c contiguously.
        assert_eq!(receiver.push(&a).unwrap().unwrap(), b"abcdefghi");
        assert!(!receiver.is_waiting());
        assert_eq!(receiver.buffered_bytes(), 0);
        // Stale duplicates after the fact are dropped.
        assert_eq!(receiver.push(&c).unwrap(), None);
        assert_eq!(receiver.push(&a).unwrap(), None);
        assert!(!receiver.is_waiting());
    }

    #[test]
    fn duplicate_of_flushed_run_is_ignored() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        let mut receiver = ReorderReceiver::new();
        assert_eq!(receiver.push(&a).unwrap().unwrap(), b"abc");
        assert_eq!(receiver.push(&a).unwrap(), None);
    }

    #[test]
    fn reorder_window_overflow_is_fatal() {
        let mut receiver = ReorderReceiver::new();
        let mut overflowed = false;
        for cursor in 1..=1_000_000u64 {
            // A stream of far-ahead chunks (offset jumps), never filling the
            // hole at 0. Buffers chunks until the window cap trips.
            let mut frame = Vec::new();
            frame.extend_from_slice(&cursor.to_be_bytes());
            frame.extend_from_slice(b"x");
            if receiver.push(&frame).is_err() {
                overflowed = true;
                break;
            }
        }
        assert!(overflowed, "reorder window should overflow");
        assert!(receiver.is_waiting(), "hole at offset 0 must still be open");
    }

    #[test]
    fn gaps_are_rejected_and_duplicates_are_ignored() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        let b = send.encode(b"def").unwrap();
        let c = send.encode(b"ghi").unwrap();
        // Strict decoder (legacy path) still rejects reorder.
        let mut receiver = StreamSequence::default();
        receiver.decode(&a).unwrap();
        assert!(receiver.decode(&c).is_err());
        let mut receiver = StreamSequence::default();
        assert_eq!(receiver.decode(&a).unwrap(), b"abc");
        assert!(receiver.decode(&a).unwrap().is_empty());
        assert_eq!(receiver.decode(&b).unwrap(), b"def");
        assert!(receiver.decode(&b).unwrap().is_empty());
        assert_eq!(receiver.decode(&c).unwrap(), b"ghi");
    }

    #[test]
    fn encode_at_pinpoints_suffix_offsets() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        let b = send.encode(b"def").unwrap();
        assert_eq!(send.offset(), 6);
        // Replay of the same suffix at its real offset carries matching bytes.
        let replay = StreamSequence::encode_at(3, b"def").unwrap();
        assert_eq!(replay, b);
        // Replay starting at an explicit offset inside the suffix.
        let tail = StreamSequence::encode_at(4, b"ef").unwrap();
        let mut receiver = ReorderReceiver::new();
        receiver.push(&a).unwrap();
        assert!(receiver.push(&b).unwrap().is_some());
        assert!(!receiver.is_waiting());
        // Stale replay beyond the consumed range is ignored, not fatal.
        assert_eq!(receiver.push(&tail).unwrap(), None);
        assert!(!receiver.is_waiting());
    }

    #[test]
    fn missing_offset_tracks_the_open_hole() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        send.encode(b"def").unwrap();
        let c = send.encode(b"ghi").unwrap();
        let mut receiver = ReorderReceiver::new();
        assert_eq!(receiver.missing_offset(), None);
        receiver.push(&c).unwrap();
        assert_eq!(receiver.missing_offset(), Some(0));
        receiver.push(&a).unwrap();
        assert!(receiver.is_waiting());
        // b (offset 3) is still owed after a filled the hole at 0.
        assert_eq!(receiver.missing_offset(), Some(3));
    }

    #[test]
    fn malformed_and_overflow_offsets_are_rejected() {
        assert!(StreamSequence::default().decode(b"short").is_err());
        let mut sequence = StreamSequence { offset: u64::MAX };
        assert!(sequence.encode(b"x").is_err());
        assert!(ReorderReceiver::new().push(b"short").is_err());
    }
}
