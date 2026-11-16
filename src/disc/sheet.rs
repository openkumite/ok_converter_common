//! The sheets that tell an image's tracks: a `.cue` (with its `.bin`/`.wav` files), CloneCD's `.ccd` (with its
//! `.img`), Alcohol's `.mds` (with its `.mdf`; the layout libmirage reads).

use super::{Folder, Region, Stored, Track, find, loose, size, stem};
use crate::io::DiscFile;
use anyhow::{Context, Result, bail};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// `mm:ss:ff` in sectors.
fn msf(s: &str) -> Result<u64> {
    let p: Vec<u64> = s
        .split(':')
        .map(|x| x.trim().parse::<u64>())
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("{s:?} is not mm:ss:ff"))?;
    match p[..] {
        [m, s, f] => Ok((m * 60 + s) * 75 + f),
        _ => bail!("{s:?} is not mm:ss:ff"),
    }
}

/// A cue line's words: `FILE "a b.bin" BINARY` is FILE, a b.bin, BINARY.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('"') {
            let (w, r) = r.split_once('"').unwrap_or((r, ""));
            out.push(w.to_string());
            rest = r.trim_start();
        } else {
            let (w, r) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            out.push(w.to_string());
            rest = r.trim_start();
        }
    }
    out
}

struct CueTrack {
    number: u8,
    mode: String,
    index0: Option<u64>,
    index1: Option<u64>,
    pregap: u64,
}

struct CueFile {
    name: String,
    kind: String,
    tracks: Vec<CueTrack>,
}

fn parse_cue(text: &str) -> Result<Vec<CueFile>> {
    let mut files: Vec<CueFile> = Vec::new();
    for line in text.lines() {
        let w = words(line);
        let Some(key) = w.first() else { continue };
        match key.to_ascii_uppercase().as_str() {
            "FILE" => {
                let name = w.get(1).context("a FILE line names no file")?.clone();
                let kind = w.get(2).map_or("BINARY".to_string(), |k| k.to_ascii_uppercase());
                files.push(CueFile { name, kind, tracks: Vec::new() });
            }
            "TRACK" => {
                let file = files.last_mut().context("a TRACK before any FILE")?;
                let number = w.get(1).and_then(|n| n.parse().ok()).context("a TRACK line has no number")?;
                let mode = w.get(2).context("a TRACK line has no mode")?.to_ascii_uppercase();
                file.tracks.push(CueTrack { number, mode, index0: None, index1: None, pregap: 0 });
            }
            "INDEX" => {
                let track = files.last_mut().and_then(|f| f.tracks.last_mut()).context("an INDEX before any TRACK")?;
                let at = msf(w.get(2).context("an INDEX line has no time")?)?;
                match w.get(1).and_then(|n| n.parse::<u32>().ok()) {
                    Some(0) => track.index0 = Some(at),
                    Some(1) => track.index1 = Some(at),
                    _ => {}
                }
            }
            "PREGAP" => {
                let track = files.last_mut().and_then(|f| f.tracks.last_mut()).context("a PREGAP before any TRACK")?;
                track.pregap = msf(w.get(1).context("a PREGAP line has no time")?)?;
            }
            _ => {}
        }
    }
    Ok(files)
}

/// Whether the `.cue` `cue` names `image` (or the `.bin` an `.ecm` of that name holds).
pub fn cue_names(folder: &dyn Folder, cue: &str, image: &str) -> Result<bool> {
    let Some(file) = folder.file(cue) else { return Ok(false) };
    let mut text = Vec::new();
    file.open()?.take(1 << 20).read_to_end(&mut text)?;
    let Ok(files) = parse_cue(&String::from_utf8_lossy(&text)) else { return Ok(false) };
    let want = loose(image);
    Ok(files.iter().any(|f| loose(&f.name) == want || format!("{}.ecm", loose(&f.name)) == want))
}

/// A track mode's sectors: audio, what they keep, their size in the file.
fn mode(m: &str) -> Result<(bool, Stored, u64)> {
    Ok(match m {
        "AUDIO" => (true, Stored::Raw, 2352),
        "MODE1/2352" | "MODE2/2352" => (false, Stored::Raw, 2352),
        "CDG" => (true, Stored::Raw, 2448),
        "MODE2/2336" | "CDI/2336" => (false, Stored::Mode2, 2336),
        "MODE1/2048" | "MODE2/2048" => (false, Stored::Data, 2048),
        other => bail!("track mode {other} is not one this reads"),
    })
}

/// A `.wav`'s samples: where they start (its `data` chunk).
fn wav_data(file: &dyn DiscFile) -> Result<u64> {
    let mut f = file.open()?;
    let mut riff = [0; 12];
    f.read_exact(&mut riff)?;
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        bail!("{} is not a WAVE file", file.name());
    }
    let mut at = 12u64;
    loop {
        let mut chunk = [0; 8];
        f.seek(SeekFrom::Start(at))?;
        f.read_exact(&mut chunk).with_context(|| format!("{} has no data chunk", file.name()))?;
        let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
        if &chunk[..4] == b"data" {
            return Ok(at + 8);
        }
        at += 8 + len + (len & 1);
    }
}

/// The tracks of the `.cue` `text` (named `cue`), its files in `folder`.
pub fn cue(folder: &dyn Folder, text: &str, cue: &str) -> Result<Vec<Track>> {
    let files = parse_cue(text)?;
    if files.iter().all(|f| f.tracks.is_empty()) {
        bail!("{cue} names no tracks");
    }
    let mut tracks = Vec::new();
    let mut lba = 0u64;
    for f in &files {
        let file = find(folder, &f.name)?.with_context(|| format!("{cue} names {}, which is not beside it", f.name))?;
        let (offset, swap) = match f.kind.as_str() {
            "BINARY" => (0, false),
            "MOTOROLA" => (0, true),
            "WAVE" => (wav_data(file.as_ref())?, false),
            other => bail!("{cue}: {} is {other}, which this does not read", f.name),
        };
        let bytes = size(file.as_ref())?.saturating_sub(offset);
        // each track's first sector in the file (an audio track's pregap, a data track's INDEX 01) and its bytes
        let mut starts = Vec::new();
        for t in &f.tracks {
            let (audio, _, _) = mode(&t.mode)?;
            let i1 = t.index1.with_context(|| format!("{cue}: track {} has no INDEX 01", t.number))?;
            starts.push(if audio { t.index0.unwrap_or(i1) } else { i1 });
        }
        let mut at = offset;
        let mut sector_of_at = 0u64;
        for (k, t) in f.tracks.iter().enumerate() {
            let (audio, stored, stride) = mode(&t.mode)?;
            // (the sectors before this track's start belong to the one before, at its size)
            at += starts[k].saturating_sub(sector_of_at) * if k == 0 { stride } else { mode(&f.tracks[k - 1].mode)?.2 };
            sector_of_at = starts[k];
            let end = match starts.get(k + 1) {
                Some(&next) => next.saturating_sub(starts[k]),
                None => (offset + bytes).saturating_sub(at) / stride,
            };
            let silence = if audio { t.pregap } else { 0 };
            let whole = starts[k] == 0 && starts.len() == 1;
            let name = if whole { file.name() } else { format!("{} (Track {})", stem(cue), t.number) };
            tracks.push(Track {
                number: t.number,
                audio,
                lba: lba + starts[k],
                silence,
                sectors: end,
                name,
                region: Region { file: file.clone(), offset: at, stride, stored, swap },
            });
            lba += silence;
        }
        let first = f.tracks.first().map_or(2352, |t| mode(&t.mode).map_or(2352, |m| m.2));
        lba += bytes / first;
    }
    tracks.sort_by_key(|t| t.number);
    Ok(tracks)
}

/// The tracks of the CloneCD sheet `text` (named `ccd`), its `.img` beside it.
pub fn ccd(folder: &dyn Folder, text: &str, ccd: &str) -> Result<Vec<Track>> {
    // [TRACK n]: MODE=, INDEX 0=, INDEX 1=
    let mut found: Vec<(u8, u32, Option<u64>, Option<u64>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(n) = line.strip_prefix("[TRACK ").and_then(|r| r.strip_suffix(']')) {
            found.push((n.trim().parse().with_context(|| format!("{ccd}: [TRACK {n}]"))?, 0, None, None));
            continue;
        }
        if line.starts_with('[') {
            continue;
        }
        let (Some(t), Some((k, v))) = (found.last_mut(), line.split_once('=')) else { continue };
        let v: u64 = v.trim().parse().unwrap_or(0);
        match k.trim().to_ascii_uppercase().as_str() {
            "MODE" => t.1 = v as u32,
            "INDEX 0" => t.2 = Some(v),
            "INDEX 1" => t.3 = Some(v),
            _ => {}
        }
    }
    if found.is_empty() {
        bail!("{ccd} names no tracks");
    }
    let img = format!("{}.img", stem(ccd));
    let file = find(folder, &img)?.with_context(|| format!("{ccd}'s {img} is not beside it"))?;
    let total = size(file.as_ref())? / 2352;
    found.sort_by_key(|t| t.0);
    let starts: Vec<u64> = found
        .iter()
        .map(|&(_, m, i0, i1)| if m == 0 { i0.or(i1).unwrap_or(0) } else { i1.or(i0).unwrap_or(0) })
        .collect();
    let mut tracks = Vec::new();
    for (k, &(number, m, _, _)) in found.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(total).min(total);
        let name = format!("{} (Track {number})", stem(ccd));
        let region =
            Region { file: file.clone(), offset: starts[k] * 2352, stride: 2352, stored: Stored::Raw, swap: false };
        tracks.push(Track {
            number,
            audio: m == 0,
            lba: starts[k],
            silence: 0,
            sectors: end.saturating_sub(starts[k]),
            name,
            region,
        });
    }
    Ok(tracks)
}

fn le16(d: &[u8], o: usize) -> Result<u64> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u64).context("the .mds is cut short")
}
fn le32(d: &[u8], o: usize) -> Result<u64> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64).context("the .mds is cut short")
}
fn le64(d: &[u8], o: usize) -> Result<u64> {
    d.get(o..o + 8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes).context("the .mds is cut short")
}

/// The tracks of the Alcohol sheet `file` (named `mds`), its `.mdf` beside it.
pub fn mds(folder: &dyn Folder, file: &dyn DiscFile, mds: &str) -> Result<Vec<Track>> {
    let mut d = Vec::new();
    file.open()?.take(16 << 20).read_to_end(&mut d)?;
    if d.get(16) != Some(&1) {
        bail!("{mds} is a version of the .mds format this does not read (only version 1)");
    }
    let sessions = le16(&d, 0x14)?;
    let session_at = le32(&d, 0x50)? as usize;
    let mut tracks = Vec::new();
    let mut data_file: Option<Arc<dyn DiscFile>> = None;
    for s in 0..sessions as usize {
        let sb = session_at + 24 * s;
        let blocks = *d.get(sb + 10).context("the .mds is cut short")? as usize;
        let blocks_at = le32(&d, sb + 20)? as usize;
        for b in 0..blocks {
            let tb = blocks_at + 80 * b;
            let t = d.get(tb..tb + 80).context("the .mds is cut short")?;
            let point = t[4];
            if !(1..=99).contains(&point) {
                continue;
            }
            let m = t[0] & 0x0F;
            let audio = m & 7 == 1;
            let stride = le16(t, 16)?;
            let sub = if t[1] == 8 { 96 } else { 0 };
            let stored = match stride - sub {
                2352 => Stored::Raw,
                2336 => Stored::Mode2,
                2048 => Stored::Data,
                other => bail!("{mds}: track {point} keeps {other}-byte sectors, which this does not read"),
            };
            let start = le32(t, 36)?;
            let offset = le64(t, 40)?;
            let (pregap, length) = match le32(t, 12)? as usize {
                0 => (0, None),
                extra => (le32(&d, extra)?, Some(le32(&d, extra + 4)?)),
            };
            if data_file.is_none() && le32(t, 48)? > 0 {
                let footer = le32(t, 52)? as usize;
                let name_at = le32(&d, footer)? as usize;
                let wide = le32(&d, footer + 4)? != 0;
                let raw = d.get(name_at..).context("the .mds is cut short")?;
                let name = if wide {
                    let units: Vec<u16> =
                        raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
                    String::from_utf16_lossy(&units)
                } else {
                    String::from_utf8_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap_or(raw.len())]).into_owned()
                };
                let name = match name.strip_prefix("*.") {
                    Some(e) => format!("{}.{e}", stem(mds)),
                    None => name,
                };
                data_file = Some(find(folder, &name)?.with_context(|| format!("{mds}'s {name} is not beside it"))?);
            }
            let f = data_file.clone().with_context(|| format!("{mds} names no data file"))?;
            let sectors = match length {
                Some(l) => l,
                None => size(f.as_ref())?.saturating_sub(offset) / stride,
            };
            let silence = if audio { pregap } else { 0 };
            let name = format!("{} (Track {point})", stem(mds));
            tracks.push(Track {
                number: point,
                audio,
                lba: start.saturating_sub(silence),
                silence,
                sectors,
                name,
                region: Region { file: f, offset, stride, stored, swap: false },
            });
        }
    }
    if tracks.is_empty() {
        bail!("{mds} names no tracks");
    }
    tracks.sort_by_key(|t| t.number);
    Ok(tracks)
}
