//! Disc images in the formats players keep them in, each read as the disc's tracks of raw 2352-byte sectors (what the
//! converters read: `Track` is a `DiscFile` of them):
//!
//! - `.cue` with its `.bin` files (one per track, or one for all; `.wav` too), or a `.bin` alone;
//! - `.iso`: 2048 bytes of data per sector, no more. The data track only, its sectors rebuilt as Mode 2 Form 1; what
//!   a PlayStation disc keeps in Mode 2 Form 2 sectors (XA audio, movies) and CD audio tracks are not in it
//!   (`DiscFile::whole_sectors` is false);
//! - `.chd` (MAME's compressed hunks of data, CD-ROM);
//! - CloneCD `.ccd` with its `.img`, Alcohol `.mds` with its `.mdf`;
//! - `.ecm` (a `.bin` with its error codes taken out: `.bin.ecm`, alone or named by a `.cue`);
//! - `.pbp`: a PSP EBOOT of a PlayStation disc as popstation makes them (not an encrypted one bought from a store).
//!
//! An archive (`.zip`, `.7z`, `.tar`, `.gz`, `.xz`, `.zst`) holding any of those is unpacked first into a scratch
//! store ([`Disc::open_unpacking`]; [`Disc::open_path`] uses a temporary file).
//!
//! A track holds what a split `.bin` of it would: an audio track from its pregap (INDEX 00; silence where the image
//! keeps no pregap), a data track from INDEX 01. [`Disc::open`] takes the image the player chose in a [`Folder`] (the
//! files beside it: the `.bin` a `.cue` names, the `.cue` beside a `.bin`).

mod archive;
mod chd;
mod ecm;
mod iso9660;
mod pbp;
pub mod sector;
mod sheet;
#[cfg(test)]
mod tests;

pub use archive::{ARCHIVES, TempStore, Unpacked, is_archive, unpack};
pub use iso9660::Iso;

use crate::io::{DiscFile, ReadSeek, Store};
use anyhow::{Context, Result, bail};
use sector::RAW;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

/// The extensions of the images [`Disc::open`] reads (and of the files they name), for pickers.
/// (Archives that hold them: [`ARCHIVES`].)
pub const EXTENSIONS: [&str; 12] = ["cue", "bin", "iso", "chd", "ccd", "img", "sub", "mds", "mdf", "ecm", "pbp", "wav"];

/// The files beside the image: a folder, or the files the player picked.
pub trait Folder: Send + Sync {
    /// Its files: name and size.
    fn files(&self) -> Vec<(String, u64)>;
    /// The file of that name (as `files` gives it).
    fn file(&self, name: &str) -> Option<Arc<dyn DiscFile>>;
}

/// A folder on disk.
pub struct StdFolder(pub PathBuf);

impl Folder for StdFolder {
    fn files(&self) -> Vec<(String, u64)> {
        let Ok(dir) = std::fs::read_dir(&self.0) else { return Vec::new() };
        let mut out: Vec<(String, u64)> = dir
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| !t.is_dir()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().map_or(0, |m| m.len())))
            .collect();
        out.sort();
        out
    }
    fn file(&self, name: &str) -> Option<Arc<dyn DiscFile>> {
        let p = self.0.join(name);
        p.is_file().then(|| Arc::new(crate::io::StdDisc(p)) as Arc<dyn DiscFile>)
    }
}

/// `name` as a sheet may write it: its last path part, lower case, `_` as space (`Tekken 3 (USA) (Track 1).bin` and
/// `Tekken_3_(USA)_(Track 1).bin` are one name).
fn loose(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_lowercase().replace('_', " ")
}

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// The file `name` in the folder (or its `.ecm`, decoded), matched loosely.
fn find(folder: &dyn Folder, name: &str) -> Result<Option<Arc<dyn DiscFile>>> {
    let files = folder.files();
    let wanted = loose(name);
    let hit = |w: &str| files.iter().find(|(n, _)| n == w).or_else(|| files.iter().find(|(n, _)| loose(n) == loose(w)));
    if let Some((n, _)) = hit(name) {
        return Ok(folder.file(n));
    }
    match hit(&format!("{wanted}.ecm")) {
        Some((n, _)) => match folder.file(n) {
            Some(f) => Ok(Some(Arc::new(ecm::Ecm::open(f)?))),
            None => Ok(None),
        },
        None => Ok(None),
    }
}

/// The image to open among several files (a folder, the files the player picked): a sheet (`.cue`, `.ccd`, `.mds`),
/// else a `.chd` or `.pbp`, else the image named Track 1, else the largest; else an archive (the largest).
pub fn main_image(folder: &dyn Folder) -> Option<String> {
    let files = folder.files();
    let of = |exts: &[&str]| {
        files.iter().filter(|(n, _)| exts.contains(&ext(n).as_str())).map(|(n, s)| (n.clone(), *s)).collect::<Vec<_>>()
    };
    for exts in [&["cue"][..], &["ccd"], &["mds"], &["chd"], &["pbp"]] {
        if let Some((n, _)) = of(exts).into_iter().next() {
            return Some(n);
        }
    }
    let images = of(&["bin", "iso", "img", "mdf", "ecm"]);
    let track1 = images.iter().find(|(n, _)| n.contains("Track 1)") || n.contains("Track 01)"));
    let image = track1.or_else(|| images.iter().max_by_key(|(_, s)| *s)).map(|(n, _)| n.clone());
    image.or_else(|| of(&ARCHIVES).into_iter().max_by_key(|(_, s)| *s).map(|(n, _)| n))
}

/// What a sector of the image keeps of the raw sector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stored {
    /// all 2352 bytes
    Raw,
    /// Mode 2's 2336 bytes after the sync and header
    Mode2,
    /// the 2048 bytes of data
    Data,
}

impl Stored {
    fn size(self) -> u64 {
        match self {
            Stored::Raw => 2352,
            Stored::Mode2 => 2336,
            Stored::Data => 2048,
        }
    }
}

/// Where a track's sectors lie: in `file` from `offset`, `stride` bytes apart (more than the stored size when
/// subchannel data follows each), audio samples big-endian when `swap`.
#[derive(Clone)]
pub(crate) struct Region {
    pub file: Arc<dyn DiscFile>,
    pub offset: u64,
    pub stride: u64,
    pub stored: Stored,
    pub swap: bool,
}

/// A track of the disc.
#[derive(Clone)]
pub struct Track {
    pub number: u8,
    pub audio: bool,
    /// The disc address (LBA) of its first sector here.
    pub lba: u64,
    /// The sectors of silence before those the image keeps (a pregap it does not keep).
    pub silence: u64,
    /// The sectors the image keeps.
    pub sectors: u64,
    name: String,
    region: Region,
}

impl Track {
    /// Its sectors: the silence and those kept.
    pub fn len(&self) -> u64 {
        self.silence + self.sectors
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What its sectors keep.
    pub fn stored(&self) -> Stored {
        self.region.stored
    }
}

impl DiscFile for Track {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        let r = &self.region;
        let inner = r.file.open()?;
        if r.stored == Stored::Raw && r.stride == 2352 && !r.swap && self.silence == 0 {
            return Ok(Box::new(Window { inner, start: r.offset, len: self.sectors * 2352, pos: 0, at: None }));
        }
        Ok(Box::new(View {
            track: self.clone(),
            inner,
            inner_pos: None,
            pos: 0,
            cur: None,
            buf: Box::new([0; RAW]),
            body: vec![0; r.stride as usize],
        }))
    }

    fn whole_sectors(&self) -> bool {
        self.region.stored != Stored::Data
    }
}

/// A disc image, opened.
pub struct Disc {
    /// The image's name (the file the player chose).
    pub image: String,
    /// Its format: `cue`, `bin`, `iso`, `chd`, `ccd`, `mds`, `ecm`, `pbp`.
    pub format: &'static str,
    /// Its tracks, by number; those the image does not keep are not here.
    pub tracks: Vec<Track>,
    /// The archive it was unpacked from, if it was.
    pub archive: Option<String>,
}

impl Disc {
    /// The image `name` in `folder`, with the files it names there (an archive: [`Disc::open_unpacking`]).
    pub fn open(folder: &dyn Folder, name: &str) -> Result<Disc> {
        if is_archive(name) {
            bail!("{name} is an archive: it is unpacked first (Disc::open_unpacking)");
        }
        let file = find(folder, name)?.with_context(|| format!("there is no {name}"))?;
        open_file(folder, file, name)
    }

    /// The image `name` in `folder`; an archive unpacked first into the store `scratch` gives (its disc files, the
    /// image found among them as [`main_image`] finds it; the progress in `log`). The store lasts as long as the
    /// disc's tracks.
    pub fn open_unpacking(
        folder: &dyn Folder,
        name: &str,
        scratch: &dyn Fn() -> Result<Arc<dyn Store>>,
        log: &(dyn Fn(&str) + Sync),
    ) -> Result<Disc> {
        if !is_archive(name) {
            return Disc::open(folder, name);
        }
        let file = find(folder, name)?.with_context(|| format!("there is no {name}"))?;
        let unpacked = unpack(file.as_ref(), scratch()?, log)?;
        let image = main_image(&unpacked).with_context(|| format!("{name} holds no disc image"))?;
        Ok(Disc { archive: Some(unpacked.archive.clone()), ..Disc::open(&unpacked, &image)? })
    }

    /// The image at `path`, with the files beside it; an archive unpacked into a temporary file in `scratch_dir`
    /// (removed when the disc goes).
    pub fn open_path_in(
        path: &std::path::Path,
        scratch_dir: &std::path::Path,
        log: &(dyn Fn(&str) + Sync),
    ) -> Result<Disc> {
        if !path.is_file() {
            bail!("there is no disc image at {}", path.display());
        }
        let dir = path.parent().map_or_else(
            || PathBuf::from("."),
            |d| if d.as_os_str().is_empty() { PathBuf::from(".") } else { d.to_path_buf() },
        );
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let scratch = || -> Result<Arc<dyn Store>> { Ok(Arc::new(TempStore::new(scratch_dir)?)) };
        Disc::open_unpacking(&StdFolder(dir), &name, &scratch, log)
    }

    /// The image at `path`, with the files beside it; an archive unpacked into the system's temporary folder.
    pub fn open_path(path: &std::path::Path) -> Result<Disc> {
        Disc::open_path_in(path, &std::env::temp_dir(), &|_| {})
    }

    pub fn track(&self, number: u8) -> Option<&Track> {
        self.tracks.iter().find(|t| t.number == number)
    }

    /// The data track (the first).
    pub fn data(&self) -> Result<&Track> {
        self.tracks.iter().find(|t| !t.audio).with_context(|| format!("{} has no data track", self.image))
    }

    /// Audio track `number`, when the image keeps it.
    pub fn audio(&self, number: u8) -> Option<&Track> {
        self.track(number).filter(|t| t.audio && !t.is_empty())
    }

    /// One line on it, for the log.
    pub fn summary(&self) -> String {
        let tracks: Vec<String> = self
            .tracks
            .iter()
            .map(|t| {
                let what = match (t.audio, t.region.stored) {
                    (true, _) => "audio",
                    (false, Stored::Data) => "data (2048-byte sectors: no XA audio or movies)",
                    (false, _) => "data",
                };
                format!("track {} {what}, {} sectors", t.number, t.len())
            })
            .collect();
        let from = self.archive.as_ref().map_or(String::new(), |a| format!(", in {a}"));
        format!("{} ({}{from}): {}", self.image, self.format, tracks.join("; "))
    }
}

/// The first bytes of a file (fewer when it is shorter).
fn head(file: &dyn DiscFile, n: u64) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    file.open()?.take(n).read_to_end(&mut buf).with_context(|| format!("cannot read {}", file.name()))?;
    Ok(buf)
}

fn size(file: &dyn DiscFile) -> Result<u64> {
    Ok(file.open()?.seek(SeekFrom::End(0))?)
}

fn open_file(folder: &dyn Folder, file: Arc<dyn DiscFile>, name: &str) -> Result<Disc> {
    let start = head(file.as_ref(), 64)?;
    let is = |magic: &[u8]| start.starts_with(magic);
    let image = name.rsplit(['/', '\\']).next().unwrap_or(name).to_string();
    let disc = |format, tracks| Ok(Disc { image: image.clone(), format, tracks, archive: None });
    if is(b"MComprHD") {
        return disc("chd", chd::tracks(file, &image)?);
    }
    if is(b"ECM\0") {
        let decoded: Arc<dyn DiscFile> = Arc::new(ecm::Ecm::open(file)?);
        let inner = stem(&image).to_string();
        // (a .cue beside it names the .bin it holds)
        if let Some(sheet) = sheet_beside(folder, &inner)? {
            return Ok(Disc { format: "ecm", ..sheet });
        }
        return disc("ecm", lone(folder, decoded, &inner)?);
    }
    if is(b"\0PBP") {
        return disc("pbp", pbp::tracks(file, &image)?);
    }
    if is(b"MEDIA DESCRIPTOR") {
        return disc("mds", sheet::mds(folder, file.as_ref(), &image)?);
    }
    match ext(&image).as_str() {
        "cue" => {
            return disc("cue", sheet::cue(folder, &String::from_utf8_lossy(&head(file.as_ref(), 1 << 20)?), &image)?);
        }
        "ccd" => {
            return disc("ccd", sheet::ccd(folder, &String::from_utf8_lossy(&head(file.as_ref(), 1 << 20)?), &image)?);
        }
        "mds" => bail!("{image} is not an Alcohol image descriptor (.mds)"),
        _ => {}
    }
    if let Some(sheet) = sheet_beside(folder, &image)? {
        return Ok(sheet);
    }
    let format = match ext(&image).as_str() {
        "iso" => "iso",
        _ => "bin",
    };
    disc(format, lone(folder, file, &image)?)
}

/// The sheet beside an image that names it: a `.cue` that names it (of the same name first, or a split image's:
/// `... (Track 1).bin`'s `....cue`), the `.ccd` of an `.img`, the `.mds` of an `.mdf` (of the same name). A sheet of
/// the same name for another image (`game.iso` beside `game.ccd` and `game.img`) is not this image's.
fn sheet_beside(folder: &dyn Folder, name: &str) -> Result<Option<Disc>> {
    let files = folder.files();
    let base = stem(name);
    let split = base.replace(" (Track 1)", "").replace("_(Track_1)", "").replace("_(Track 1)", "");
    for (want, sheet) in [
        (format!("{base}.cue"), "cue"),
        (format!("{base}.ccd"), "ccd"),
        (format!("{base}.mds"), "mds"),
        (format!("{split}.cue"), "cue"),
    ] {
        let own = match sheet {
            "ccd" => ext(name) == "img",
            "mds" => ext(name) == "mdf",
            _ => true,
        };
        if want == name || !own {
            continue;
        }
        let Some((n, _)) = files.iter().find(|(n, _)| loose(n) == loose(&want)) else { continue };
        if sheet != "cue" || sheet::cue_names(folder, n, name)? {
            let file = folder.file(n).with_context(|| format!("cannot open {n}"))?;
            return open_file(folder, file, n).map(Some);
        }
    }
    for (n, _) in files.iter().filter(|(n, _)| ext(n) == "cue") {
        if sheet::cue_names(folder, n, name)? {
            let file = folder.file(n).with_context(|| format!("cannot open {n}"))?;
            return open_file(folder, file, n).map(Some);
        }
    }
    Ok(None)
}

/// An image with no sheet: one data track, its sectors' size told by the data (raw 2352, 2448 with subchannel data,
/// Mode 2's 2336, or an ISO's 2048), and the audio tracks beside it named as a split image's (`... (Track 2).bin`).
fn lone(folder: &dyn Folder, file: Arc<dyn DiscFile>, name: &str) -> Result<Vec<Track>> {
    let h = head(file.as_ref(), 17 * 2448)?;
    let cd001 = |at: usize| h.get(at + 1..at + 6) == Some(b"CD001");
    let (stored, stride) = if sector::has_sync(&h)
        && h.get(2448..2460) == Some(&sector::SYNC[..])
        && h.get(2352..2364) != Some(&sector::SYNC[..])
    {
        (Stored::Raw, 2448)
    } else if sector::has_sync(&h) {
        (Stored::Raw, 2352)
    } else if cd001(16 * 2048) {
        (Stored::Data, 2048)
    } else if cd001(16 * 2336 + 8) {
        (Stored::Mode2, 2336)
    } else {
        bail!("{name} is not a disc image this reads: no CD sectors in it, and no ISO 9660 data at its start")
    };
    let sectors = size(file.as_ref())? / stride;
    let mut tracks = vec![Track {
        number: 1,
        audio: false,
        lba: 0,
        silence: 0,
        sectors,
        name: name.to_string(),
        region: Region { file, offset: 0, stride, stored, swap: false },
    }];
    for n in 2..=99u8 {
        let other = name.replace("Track 1)", &format!("Track {n})")).replace("Track 01)", &format!("Track {n:02})"));
        if other == name {
            break;
        }
        let Some(f) = find(folder, &other)? else { break };
        let sectors = size(f.as_ref())? / 2352;
        let lba = tracks.last().map_or(0, |t: &Track| t.lba + t.len());
        tracks.push(Track {
            number: n,
            audio: true,
            lba,
            silence: 0,
            sectors,
            name: f.name(),
            region: Region { file: f, offset: 0, stride: 2352, stored: Stored::Raw, swap: false },
        });
    }
    Ok(tracks)
}

/// A raw track kept as it is: a window on its file.
struct Window {
    inner: Box<dyn ReadSeek>,
    start: u64,
    len: u64,
    pos: u64,
    /// where `inner` is, when known
    at: Option<u64>,
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        if n == 0 {
            return Ok(0);
        }
        if self.at != Some(self.pos) {
            self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        }
        let got = self.inner.read(&mut buf[..n])?;
        self.pos += got as u64;
        self.at = Some(self.pos);
        Ok(got)
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = seek_to(to, self.pos, self.len)?;
        Ok(self.pos)
    }
}

fn seek_to(to: SeekFrom, pos: u64, len: u64) -> std::io::Result<u64> {
    let p = match to {
        SeekFrom::Start(p) => Some(p),
        SeekFrom::End(d) => len.checked_add_signed(d),
        SeekFrom::Current(d) => pos.checked_add_signed(d),
    };
    p.ok_or_else(|| std::io::Error::other("a seek before the start"))
}

/// A track made whole sector by sector: its silence, its sectors rebuilt from what the image keeps.
struct View {
    track: Track,
    inner: Box<dyn ReadSeek>,
    inner_pos: Option<u64>,
    pos: u64,
    /// the sector in `buf`
    cur: Option<u64>,
    buf: Box<[u8; RAW]>,
    body: Vec<u8>,
}

impl View {
    fn load(&mut self, n: u64) -> std::io::Result<()> {
        let t = &self.track;
        if n < t.silence {
            self.buf.fill(0);
            self.cur = Some(n);
            return Ok(());
        }
        let r = &t.region;
        let at = r.offset + (n - t.silence) * r.stride;
        if self.inner_pos != Some(at) {
            self.inner.seek(SeekFrom::Start(at))?;
        }
        let mut got = 0;
        while got < self.body.len() {
            match self.inner.read(&mut self.body[got..])? {
                0 => break,
                k => got += k,
            }
        }
        self.body[got..].fill(0);
        self.inner_pos = Some(at + got as u64);
        let lba = t.lba + n;
        let stored = r.stored.size() as usize;
        match r.stored {
            Stored::Raw => {
                self.buf.copy_from_slice(&self.body[..RAW]);
                if r.swap {
                    for pair in self.buf.as_chunks_mut::<2>().0 {
                        pair.swap(0, 1);
                    }
                }
            }
            Stored::Mode2 => sector::from_mode2(&mut self.buf, lba, &self.body[..stored]),
            Stored::Data => sector::from_data(&mut self.buf, lba, &self.body[..stored]),
        }
        self.cur = Some(n);
        Ok(())
    }
}

impl Read for View {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = self.track.len() * RAW as u64;
        if self.pos >= len || buf.is_empty() {
            return Ok(0);
        }
        let n = self.pos / RAW as u64;
        if self.cur != Some(n) {
            self.load(n)?;
        }
        let off = (self.pos % RAW as u64) as usize;
        let k = buf.len().min(RAW - off);
        buf[..k].copy_from_slice(&self.buf[off..off + k]);
        self.pos += k as u64;
        Ok(k)
    }
}

impl Seek for View {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = seek_to(to, self.pos, self.track.len() * RAW as u64)?;
        Ok(self.pos)
    }
}

/// A file made of blocks decoded one at a time (a CHD's hunks, a PBP's blocks): read and seek over the decoded bytes.
pub(crate) trait Blocks: Send {
    fn block_size(&self) -> u64;
    /// The decoded length.
    fn len(&self) -> u64;
    /// Decodes block `n` into `out` (`block_size` bytes; the last may be short).
    fn block(&mut self, n: u64, out: &mut Vec<u8>) -> Result<()>;
}

pub(crate) struct BlockReader<B: Blocks> {
    pub blocks: B,
    pos: u64,
    cur: Option<u64>,
    buf: Vec<u8>,
}

impl<B: Blocks> BlockReader<B> {
    pub fn new(blocks: B) -> Self {
        BlockReader { blocks, pos: 0, cur: None, buf: Vec::new() }
    }
}

impl<B: Blocks> Read for BlockReader<B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (bs, len) = (self.blocks.block_size(), self.blocks.len());
        if self.pos >= len || buf.is_empty() {
            return Ok(0);
        }
        let n = self.pos / bs;
        if self.cur != Some(n) {
            self.cur = None;
            self.blocks.block(n, &mut self.buf).map_err(std::io::Error::other)?;
            self.cur = Some(n);
        }
        let off = (self.pos - n * bs) as usize;
        let k = buf.len().min(self.buf.len().saturating_sub(off)).min((len - self.pos) as usize);
        if k == 0 {
            return Err(std::io::Error::other(format!("block {n} is shorter than it should be")));
        }
        buf[..k].copy_from_slice(&self.buf[off..off + k]);
        self.pos += k as u64;
        Ok(k)
    }
}

impl<B: Blocks> Seek for BlockReader<B> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = seek_to(to, self.pos, self.blocks.len())?;
        Ok(self.pos)
    }
}
