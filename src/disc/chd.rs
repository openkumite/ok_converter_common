//! MAME's CHD (compressed hunks of data) of a CD-ROM: its hunks hold frames of 2448 bytes (the sector's 2352, then
//! 96 of subchannel data), each track's frames padded to a multiple of 4; the tracks are told by the CHT2 (or CHTR)
//! metadata. Audio samples are big-endian. Read through the `chd` crate (pure Rust).

use super::{BlockReader, Blocks, Region, Stored, Track};
use crate::io::{DiscFile, ReadSeek};
use anyhow::{Context, Result, anyhow, bail};
use std::sync::Arc;

const FRAME: u64 = 2448;
const CHT2: u32 = u32::from_be_bytes(*b"CHT2");
const CHTR: u32 = u32::from_be_bytes(*b"CHTR");

type Chd = chd::Chd<Box<dyn ReadSeek>>;

fn open_chd(file: &dyn DiscFile) -> Result<Chd> {
    match chd::Chd::open(file.open()?, None) {
        Ok(c) => Ok(c),
        Err(chd::Error::RequiresParent) => {
            bail!("{} needs its parent CHD (it holds only the differences); give the whole one", file.name())
        }
        Err(e) => Err(anyhow!("{} is not a CHD this reads: {e}", file.name())),
    }
}

/// The CHD's hunks, decompressed, as one file.
struct ChdFile {
    file: Arc<dyn DiscFile>,
    name: String,
}

struct Hunks {
    chd: Chd,
    hunk: u64,
    len: u64,
    compressed: Vec<u8>,
}

impl Blocks for Hunks {
    fn block_size(&self) -> u64 {
        self.hunk
    }
    fn len(&self) -> u64 {
        self.len
    }
    fn block(&mut self, n: u64, out: &mut Vec<u8>) -> Result<()> {
        out.resize(self.hunk as usize, 0);
        let mut h = self.chd.hunk(n as u32).map_err(|e| anyhow!("hunk {n}: {e}"))?;
        h.read_hunk_in(&mut self.compressed, out).map_err(|e| anyhow!("hunk {n}: {e}"))?;
        Ok(())
    }
}

impl DiscFile for ChdFile {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        let chd = open_chd(self.file.as_ref())?;
        let (hunk, count) = (chd.header().hunk_size() as u64, chd.header().hunk_count() as u64);
        Ok(Box::new(BlockReader::new(Hunks { chd, hunk, len: hunk * count, compressed: Vec::new() })))
    }
}

/// `KEY:value` of a track's metadata text.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split_whitespace().find_map(|w| w.strip_prefix(key)?.strip_prefix(':'))
}

/// The tracks of the CHD `file`.
pub fn tracks(file: Arc<dyn DiscFile>, name: &str) -> Result<Vec<Track>> {
    let mut chd = open_chd(file.as_ref())?;
    if !(chd.header().hunk_size() as u64).is_multiple_of(FRAME) {
        bail!("{name} is not a CD-ROM CHD (its hunks are not whole CD frames)");
    }
    let metadata: Vec<chd::metadata::Metadata> =
        chd.metadata_refs().try_into().map_err(|e| anyhow!("{name}: its metadata: {e}"))?;
    let mut found: Vec<(u8, String)> = Vec::new();
    for m in metadata.iter().filter(|m| m.metatag == CHT2 || m.metatag == CHTR) {
        let text = String::from_utf8_lossy(&m.value).trim_end_matches('\0').to_string();
        let n = field(&text, "TRACK")
            .and_then(|n| n.parse().ok())
            .with_context(|| format!("{name}: a track without a number ({text})"))?;
        found.push((n, text));
    }
    if found.is_empty() {
        bail!("{name} is not a CD-ROM CHD (no track metadata: a hard disk or GD-ROM image?)");
    }
    found.sort_by_key(|t| t.0);
    let stream: Arc<dyn DiscFile> = Arc::new(ChdFile { file, name: name.to_string() });
    let mut tracks = Vec::new();
    let (mut frame, mut lba) = (0u64, 0u64);
    for (number, text) in &found {
        let num = |k: &str| field(text, k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let frames = num("FRAMES");
        let pregap = num("PREGAP");
        let pregap_kept = field(text, "PGTYPE").is_some_and(|t| t.starts_with('V'));
        let ty = field(text, "TYPE").unwrap_or_default();
        let (audio, stored) = match ty {
            "AUDIO" => (true, Stored::Raw),
            "MODE1_RAW" | "MODE2_RAW" => (false, Stored::Raw),
            "MODE2" | "MODE2_FORM_MIX" => (false, Stored::Mode2),
            "MODE1" | "MODE2_FORM1" => (false, Stored::Data),
            other => bail!("{name}: track {number} is {other}, which this does not read"),
        };
        // an audio track from its pregap (silence when the CHD does not keep it), a data track from INDEX 01
        let (skip, silence) = match (audio, pregap_kept) {
            (true, true) => (0, 0),
            (true, false) => (0, pregap),
            (false, true) => (pregap, 0),
            (false, false) => (0, 0),
        };
        let lba_here = if pregap_kept { lba + skip } else { lba + pregap - silence };
        tracks.push(Track {
            number: *number,
            audio,
            lba: lba_here,
            silence,
            sectors: frames.saturating_sub(skip),
            name: format!("{name} (Track {number})"),
            region: Region { file: stream.clone(), offset: (frame + skip) * FRAME, stride: FRAME, stored, swap: audio },
        });
        frame += frames.div_ceil(4) * 4;
        lba += frames + if pregap_kept { 0 } else { pregap };
    }
    Ok(tracks)
}
