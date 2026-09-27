//! CSQPX2 integrity checks. A gap/reorder terminates the TCP stream; a
//! retransmitted duplicate copy of already-consumed bytes is dropped without
//! advancing the cursor. Separate cursors are used for the two directions.
use anyhow::{Result, bail};

#[derive(Default)]
pub struct StreamSequence {
    offset: u64,
}

impl StreamSequence {
    pub fn encode(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(8 + bytes.len());
        result.extend_from_slice(&self.offset.to_be_bytes());
        result.extend_from_slice(bytes);
        self.advance(bytes.len())?;
        Ok(result)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_bytes_round_trip() {
        let (mut send, mut receive) = (StreamSequence::default(), StreamSequence::default());
        for bytes in [b"abc".as_slice(), b"defgh", b"i"] {
            assert_eq!(receive.decode(&send.encode(bytes).unwrap()).unwrap(), bytes);
        }
    }

    #[test]
    fn gaps_are_rejected_and_duplicates_are_ignored() {
        let mut send = StreamSequence::default();
        let a = send.encode(b"abc").unwrap();
        let b = send.encode(b"def").unwrap();
        let c = send.encode(b"ghi").unwrap();
        // A chunk beyond the expected offset is a gap/reorder: fatal.
        let mut receiver = StreamSequence::default();
        receiver.decode(&a).unwrap();
        assert!(receiver.decode(&c).is_err());
        // A retransmitted copy of an already-consumed chunk yields empty bytes.
        let mut receiver = StreamSequence::default();
        assert_eq!(receiver.decode(&a).unwrap(), b"abc");
        assert!(receiver.decode(&a).unwrap().is_empty());
        assert_eq!(receiver.decode(&b).unwrap(), b"def");
        assert!(receiver.decode(&b).unwrap().is_empty());
        assert_eq!(receiver.decode(&c).unwrap(), b"ghi");
    }

    #[test]
    fn malformed_and_overflow_offsets_are_rejected() {
        assert!(StreamSequence::default().decode(b"short").is_err());
        let mut sequence = StreamSequence { offset: u64::MAX };
        assert!(sequence.encode(b"x").is_err());
    }
}
