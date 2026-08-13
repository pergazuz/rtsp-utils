//! Picks the right parser by looking at the file itself, so callers publish a
//! path without declaring what format it holds.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::jpeg::JpegProbe;
use super::mp4::Mp4Probe;
use crate::domain::media::MediaSource;
use crate::domain::ports::MediaProbe;
use crate::domain::Result;

pub struct AutoProbe;

impl MediaProbe for AutoProbe {
    fn probe(&self, path: &Path, name: &str) -> Result<MediaSource> {
        // Sniff the signature rather than trusting the extension: a JPEG
        // opens with the SOI marker, and everything else goes to the MOV/MP4
        // parser, whose own errors describe what went wrong with the file.
        let mut magic = [0u8; 2];
        let is_jpeg = File::open(path)?.read_exact(&mut magic).is_ok() && magic == [0xff, 0xd8];
        if is_jpeg {
            JpegProbe.probe(path, name)
        } else {
            Mp4Probe.probe(path, name)
        }
    }
}
