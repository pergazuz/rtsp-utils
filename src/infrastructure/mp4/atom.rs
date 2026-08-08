//! Minimal ISO-BMFF / QuickTime atom walker.
//!
//! Everything here works over an in-memory slice: we only ever load `moov`
//! into memory, never the media data.

use crate::domain::{Error, Result};

/// A single box: its four-character type and its payload (header stripped).
#[derive(Debug, Clone, Copy)]
pub struct Atom<'a> {
    pub kind: [u8; 4],
    pub body: &'a [u8],
}

/// Iterates the boxes laid out back to back in `body`.
pub struct Atoms<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Atoms<'a> {
    type Item = Atom<'a>;

    fn next(&mut self) -> Option<Atom<'a>> {
        // A trailing runt smaller than a header is padding; stop cleanly.
        if self.rest.len() < 8 {
            return None;
        }
        let size32 = u32::from_be_bytes([self.rest[0], self.rest[1], self.rest[2], self.rest[3]]);
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&self.rest[4..8]);

        let (header, size) = match size32 {
            // 1 => the real size follows the type as a 64-bit value.
            1 => {
                if self.rest.len() < 16 {
                    return None;
                }
                let mut b = [0u8; 8];
                b.copy_from_slice(&self.rest[8..16]);
                (16usize, u64::from_be_bytes(b) as usize)
            }
            // 0 => the box runs to the end of its container.
            0 => (8usize, self.rest.len()),
            n => (8usize, n as usize),
        };

        if size < header || size > self.rest.len() {
            // Truncated or nonsensical: consume the remainder and stop.
            self.rest = &[];
            return None;
        }

        let body = &self.rest[header..size];
        self.rest = &self.rest[size..];
        Some(Atom { kind, body })
    }
}

pub fn children(body: &[u8]) -> Atoms<'_> {
    Atoms { rest: body }
}

/// First child of `body` with the given type.
pub fn find<'a>(body: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(body).find(|a| &a.kind == kind).map(|a| a.body)
}

/// Follows a chain of nested box types, e.g. `["mdia", "minf", "stbl"]`.
pub fn find_path<'a>(body: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut current = body;
    for kind in path {
        current = find(current, kind)?;
    }
    Some(current)
}

pub fn find_all<'a>(body: &'a [u8], kind: [u8; 4]) -> Vec<&'a [u8]> {
    children(body)
        .filter(|a| a.kind == kind)
        .map(|a| a.body)
        .collect()
}

// ---- primitive readers ------------------------------------------------------

pub fn u8_at(b: &[u8], at: usize) -> Result<u8> {
    b.get(at).copied().ok_or_else(|| short(at, 1, b.len()))
}

pub fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    let s = b.get(at..at + 2).ok_or_else(|| short(at, 2, b.len()))?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

pub fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    let s = b.get(at..at + 4).ok_or_else(|| short(at, 4, b.len()))?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn i32_at(b: &[u8], at: usize) -> Result<i32> {
    Ok(u32_at(b, at)? as i32)
}

pub fn u64_at(b: &[u8], at: usize) -> Result<u64> {
    let s = b.get(at..at + 8).ok_or_else(|| short(at, 8, b.len()))?;
    Ok(u64::from_be_bytes([
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
    ]))
}

/// Version byte of a FullBox (the flags occupy the following three bytes).
pub fn full_box_version(b: &[u8]) -> Result<u8> {
    u8_at(b, 0)
}

fn short(at: usize, want: usize, have: usize) -> Error {
    Error::MalformedContainer(format!(
        "box truncated: needed {want} bytes at offset {at} but the box is {have} bytes"
    ))
}
