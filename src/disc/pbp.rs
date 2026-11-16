//! A PSP EBOOT.PBP of a PlayStation disc, as popstation makes them: the PBP's DATA.PSAR holds the disc
//! (`PSISOIMG0000`; several discs: `PSTITLEIMG000000`, the first is read), its table of contents at 0x800, the index
//! of its blocks at 0x4000 (32 bytes each: offset, length), the blocks from 0x100000: 16 raw sectors each (0x9300
//! bytes), deflated unless their length is 0x9300. An EBOOT bought from a store is encrypted; it is not read.

use super::{BlockReader, Blocks, Region, Stored, Track};
use crate::io::{DiscFile, ReadSeek};
use anyhow::{Context, Result, bail};
use std::io::{Read, SeekFrom};
use std::sync::Arc;

const BLOCK: u64 = 0x9300;
const SECTORS_PER_BLOCK: u64 = 16;

fn le32(d: &[u8], o: usize) -> u64 {
    d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64)
}

fn read_at(f: &mut dyn ReadSeek, at: u64, n: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(at))?;
    let mut buf = Vec::with_capacity(n);
    f.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn from_bcd(b: u8) -> u64 {
    (b >> 4) as u64 * 10 + (b & 0x0F) as u64
}

fn msf(m: u8, s: u8, f: u8) -> u64 {
    (from_bcd(m) * 60 + from_bcd(s)) * 75 + from_bcd(f)
}

/// The disc in the PBP, its blocks decoded: raw sectors from the disc's start.
struct PbpFile {
    file: Arc<dyn DiscFile>,
    name: String,
    /// where the blocks start in the PBP
    base: u64,
    blocks: Arc<Vec<(u64, u64)>>,
    len: u64,
}

struct PbpBlocks {
    inner: Box<dyn ReadSeek>,
    base: u64,
    blocks: Arc<Vec<(u64, u64)>>,
    len: u64,
    name: String,
}

impl Blocks for PbpBlocks {
    fn block_size(&self) -> u64 {
        BLOCK
    }
    fn len(&self) -> u64 {
        self.len
    }
    fn block(&mut self, n: u64, out: &mut Vec<u8>) -> Result<()> {
        let &(offset, length) = self.blocks.get(n as usize).with_context(|| format!("{}: no block {n}", self.name))?;
        let data = read_at(self.inner.as_mut(), self.base + offset, length as usize)?;
        out.clear();
        if length == BLOCK {
            out.extend_from_slice(&data);
        } else {
            let raw = flate2::read::DeflateDecoder::new(&data[..]).take(BLOCK).read_to_end(out);
            if raw.is_err() || out.len() as u64 != BLOCK {
                out.clear();
                flate2::read::ZlibDecoder::new(&data[..]).take(BLOCK).read_to_end(out).with_context(|| {
                    format!("{}: block {n} does not decompress (is the EBOOT encrypted?)", self.name)
                })?;
            }
        }
        out.resize(BLOCK as usize, 0);
        Ok(())
    }
}

impl DiscFile for PbpFile {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        let blocks = PbpBlocks {
            inner: self.file.open()?,
            base: self.base,
            blocks: self.blocks.clone(),
            len: self.len,
            name: self.name.clone(),
        };
        Ok(Box::new(BlockReader::new(blocks)))
    }
}

/// The tracks of the PBP `file` (named `name`).
pub fn tracks(file: Arc<dyn DiscFile>, name: &str) -> Result<Vec<Track>> {
    let mut f = file.open()?;
    let head = read_at(f.as_mut(), 0, 0x28)?;
    let psar = le32(&head, 0x24);
    if psar == 0 {
        bail!("{name} is a PBP with no DATA.PSAR (not a PlayStation disc)");
    }
    let magic = read_at(f.as_mut(), psar, 16)?;
    let iso = if magic.starts_with(b"PSISOIMG0000") {
        psar
    } else if magic.starts_with(b"PSTITLEIMG000000") {
        let discs = read_at(f.as_mut(), psar + 0x200, 20)?;
        let first = (0..5)
            .map(|k| le32(&discs, 4 * k))
            .find(|&o| o != 0)
            .with_context(|| format!("{name}: its disc table is empty"))?;
        psar + first
    } else if magic.starts_with(b"\0PGD") {
        bail!(
            "{name} is encrypted (an EBOOT from a store); make one from your disc with popstation, or give the disc image itself"
        );
    } else {
        bail!("{name} holds no PlayStation disc (no PSISOIMG0000 in its DATA.PSAR)");
    };
    let header = read_at(f.as_mut(), iso, 0x4000)?;
    if header.get(0x400..0x404) == Some(b"\0PGD") {
        bail!(
            "{name} is encrypted (an EBOOT from a store); make one from your disc with popstation, or give the disc image itself"
        );
    }
    // the blocks
    let index = read_at(f.as_mut(), iso + 0x4000, (0x100000 - 0x4000) as usize)?;
    let blocks: Vec<(u64, u64)> = index
        .as_chunks::<32>()
        .0
        .iter()
        .map(|e| (le32(e, 0), le32(e, 4) & 0xFFFF))
        .take_while(|&(o, l)| o != 0 || l != 0)
        .collect();
    if blocks.is_empty() {
        bail!("{name}: its disc has no blocks");
    }
    // the table of contents: A0, A1, A2, then the tracks (10 bytes each; the start's MSF at bytes 7-9, or 3-5)
    let toc: Vec<&[u8; 10]> = header
        .get(0x800..0x800 + 10 * 102)
        .context("a short PBP")?
        .as_chunks::<10>()
        .0
        .iter()
        .take_while(|e| e[2] != 0)
        .collect();
    let point = |p: u8| toc.iter().find(|e| e[2] == p);
    let lead_out = point(0xA2).map(|e| msf(e[7], e[8], e[9]).saturating_sub(150));
    let mut entries: Vec<(u8, bool, &[u8; 10])> = toc
        .iter()
        .filter(|e| (1..=0x99).contains(&e[2]))
        .map(|e| (from_bcd(e[2]) as u8, e[0] & 0x40 == 0, *e))
        .collect();
    entries.sort_by_key(|e| e.0);
    let at =
        if entries.first().is_some_and(|e| msf(e.2[3], e.2[4], e.2[5]) == 150 && msf(e.2[7], e.2[8], e.2[9]) != 150) {
            3
        } else {
            7
        };
    let kept = blocks.len() as u64 * SECTORS_PER_BLOCK;
    let total = lead_out.unwrap_or(kept).min(kept);
    let stream: Arc<dyn DiscFile> = Arc::new(PbpFile {
        file,
        name: format!("{name} (disc)"),
        base: iso + 0x100000,
        blocks: Arc::new(blocks),
        len: total * 2352,
    });
    if entries.is_empty() {
        entries.push((1, false, &[0; 10]));
    }
    // a track's sectors: from INDEX 01 (data) or its pregap (audio: 2 seconds, which the table does not tell)
    let starts: Vec<u64> = entries
        .iter()
        .enumerate()
        .map(|(k, &(_, audio, e))| {
            let i1 = if k == 0 && !audio { 0 } else { msf(e[at], e[at + 1], e[at + 2]).saturating_sub(150) };
            if audio && k > 0 { i1.saturating_sub(150) } else { i1 }
        })
        .collect();
    let mut tracks = Vec::new();
    for (k, &(number, audio, _)) in entries.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(total).min(total);
        if starts[k] >= end {
            continue;
        }
        let region =
            Region { file: stream.clone(), offset: starts[k] * 2352, stride: 2352, stored: Stored::Raw, swap: false };
        tracks.push(Track {
            number,
            audio,
            lba: starts[k],
            silence: 0,
            sectors: end - starts[k],
            name: format!("{name} (Track {number})"),
            region,
        });
    }
    Ok(tracks)
}
