//! A private storage representation; logical bytes and their hashes never change.
use crate::error::{CalmError, Result};
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use std::{borrow::Cow, io::Write};

pub(crate) const HEADER_BYTES: usize = 12;
pub(crate) const MAX_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const MIN_BLOCK_BYTES: usize = 4096;
const MAGIC: &[u8; 4] = b"NCZ\x01";

fn malformed() -> CalmError {
    CalmError::Internal("invalid compact storage block".into())
}

/// Shared bounded primitive; each storage owner chooses its eligibility and
/// envelope while retaining one strict zlib implementation.
pub(crate) fn compress(raw: &[u8], level: u32) -> Result<Vec<u8>> {
    if raw.len() > MAX_BLOCK_BYTES {
        return Err(malformed());
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(raw)?;
    Ok(encoder.finish()?)
}

pub(crate) fn decompress(input: &[u8], length: usize) -> Result<Vec<u8>> {
    if length > MAX_BLOCK_BYTES || input.len() > MAX_BLOCK_BYTES {
        return Err(malformed());
    }
    // One extra output byte detects an incorrect declared size. Require an
    // entire, single stream including its checksum and consume all input.
    let mut out = vec![0; length + 1];
    let mut decoder = Decompress::new(true);
    let status = decoder
        .decompress(input, &mut out, FlushDecompress::Finish)
        .map_err(|_| malformed())?;
    if status != Status::StreamEnd
        || decoder.total_out() != length as u64
        || decoder.total_in() != input.len() as u64
    {
        return Err(malformed());
    }
    out.truncate(length);
    Ok(out)
}

/// Large/incompressible values stay plain; the bound limits this encoding,
/// not the logical objects accepted by their existing owners.
pub(crate) fn pack(raw: &[u8]) -> Result<Option<Vec<u8>>> {
    if !(MIN_BLOCK_BYTES..=MAX_BLOCK_BYTES).contains(&raw.len()) {
        return Ok(None);
    }
    let data = compress(raw, 1)?;
    if HEADER_BYTES + data.len() >= raw.len() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(HEADER_BYTES + data.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(raw.len() as u64).to_be_bytes());
    out.extend_from_slice(&data);
    Ok(Some(out))
}

/// A recognized event frame never falls back to the legacy representation,
/// including truncated or future headers.
pub(crate) fn logical_len(prefix: &[u8], stored_len: u64) -> Result<u64> {
    if !prefix.starts_with(b"NCZ") {
        return Ok(stored_len);
    }
    if prefix.len() < HEADER_BYTES || &prefix[..4] != MAGIC || stored_len <= HEADER_BYTES as u64 {
        return Err(malformed());
    }
    let length = u64::from_be_bytes(
        prefix[4..HEADER_BYTES]
            .try_into()
            .map_err(|_| malformed())?,
    );
    if length > MAX_BLOCK_BYTES as u64 || length < MIN_BLOCK_BYTES as u64 {
        return Err(malformed());
    }
    Ok(length)
}

pub(crate) fn unpack(stored: &[u8]) -> Result<Cow<'_, [u8]>> {
    let length = logical_len(stored, stored.len() as u64)?;
    if !stored.starts_with(b"NCZ") {
        return Ok(Cow::Borrowed(stored));
    }
    Ok(Cow::Owned(decompress(
        &stored[HEADER_BYTES..],
        length as usize,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_blocks_roundtrip_and_keep_plain_inputs() {
        let raw = b"repetitive historical entry ".repeat(2048);
        let stored = pack(&raw).unwrap().unwrap();
        assert!(stored.len() < raw.len() / 2);
        assert_eq!(
            logical_len(&stored[..HEADER_BYTES], stored.len() as u64).unwrap(),
            raw.len() as u64
        );
        assert_eq!(unpack(&stored).unwrap().as_ref(), raw);
        assert_eq!(
            unpack(b"legacy plain JSON").unwrap().as_ref(),
            b"legacy plain JSON"
        );
        assert!(pack(b"small").unwrap().is_none());
        assert!(pack(&vec![b'x'; MIN_BLOCK_BYTES - 1]).unwrap().is_none());
        let minimum = vec![b'x'; MIN_BLOCK_BYTES];
        assert_eq!(
            unpack(&pack(&minimum).unwrap().unwrap()).unwrap().as_ref(),
            minimum
        );
        let large_plain = vec![0; MAX_BLOCK_BYTES + 1];
        assert!(pack(&large_plain).unwrap().is_none());
        assert!(matches!(unpack(&large_plain).unwrap(), Cow::Borrowed(_)));
    }

    #[test]
    fn invalid_compact_blocks_never_fall_back_or_expand_without_a_bound() {
        let raw = b"bounded historical body ".repeat(2048);
        let valid = pack(&raw).unwrap().unwrap();
        let mut cases = vec![
            valid[..HEADER_BYTES - 1].to_vec(),
            valid[..valid.len() - 1].to_vec(),
        ];
        for length in [
            0,
            raw.len() as u64 - 1,
            raw.len() as u64 + 1,
            MAX_BLOCK_BYTES as u64 + 1,
        ] {
            let mut bad = valid.clone();
            bad[4..12].copy_from_slice(&length.to_be_bytes());
            cases.push(bad);
        }
        let mut future = valid.clone();
        future[3] = 2;
        cases.push(future);
        let mut trailing = valid.clone();
        trailing.push(0);
        cases.push(trailing);
        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(&valid[HEADER_BYTES..]);
        cases.push(concatenated);
        let mut corrupt = valid.clone();
        let last = corrupt.len() - 1;
        corrupt[last] ^= 1;
        cases.push(corrupt);
        for bad in cases {
            assert!(unpack(&bad).is_err());
        }
    }

    #[test]
    fn incompressible_bytes_are_not_encoded() {
        let mut state = 7u64;
        let bytes: Vec<_> = (0..16384)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 56) as u8
            })
            .collect();
        assert!(pack(&bytes).unwrap().is_none());
    }
}
