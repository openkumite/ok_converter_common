//! Disc images in archives: `.zip`, `.7z`, `.tar`, and one file compressed alone or a tarball (`.gz`, `.xz`, `.zst`;
//! `.tgz`, `.txz`, `.tzst`). A compressed image cannot be read at random places cheaply, so its disc files are
//! unpacked once, one after another, into a scratch `Store` (a temporary file; the browser's storage), and read from
//! there: [`Unpacked`] is the folder of them, where [`super::Disc::open`] finds the image as among any files.
//! All through pure Rust crates (zip, sevenz-rust2, tar, flate2, lzma-rust2, ruzstd): the browser build reads them too.

use super::{EXTENSIONS, Folder, ext};
use crate::io::{DiscFile, ReadSeek, Store};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

/// The extensions of the archives [`unpack`] reads.
pub const ARCHIVES: [&str; 9] = ["zip", "7z", "tar", "gz", "tgz", "xz", "txz", "zst", "tzst"];

/// Whether `name` is an archive [`unpack`] reads.
pub fn is_archive(name: &str) -> bool {
    ARCHIVES.contains(&ext(name).as_str())
}

/// The disc files of an archive, unpacked into a scratch store: a folder of them (by name, without the archive's
/// folders). They last as long as the store does.
pub struct Unpacked {
    /// The archive's name.
    pub archive: String,
    files: BTreeMap<String, (u64, u64)>,
    store: Arc<dyn Store>,
}

impl Folder for Unpacked {
    fn files(&self) -> Vec<(String, u64)> {
        self.files.iter().map(|(n, &(_, len))| (n.clone(), len)).collect()
    }
    fn file(&self, name: &str) -> Option<Arc<dyn DiscFile>> {
        let &(at, len) = self.files.get(name)?;
        Some(Arc::new(Part { name: name.to_string(), store: self.store.clone(), at, len }))
    }
}

/// An unpacked file: its bytes in the store.
struct Part {
    name: String,
    store: Arc<dyn Store>,
    at: u64,
    len: u64,
}

impl DiscFile for Part {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(std::io::BufReader::with_capacity(
            1 << 16,
            PartReader { store: self.store.clone(), at: self.at, len: self.len, pos: 0 },
        )))
    }
}

struct PartReader {
    store: Arc<dyn Store>,
    at: u64,
    len: u64,
    pos: u64,
}

impl Read for PartReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.store.read_at(self.at + self.pos, &mut buf[..n]).map_err(std::io::Error::other)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for PartReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = super::seek_to(to, self.pos, self.len)?;
        Ok(self.pos)
    }
}

/// Writes the files it is given one after another into the store.
struct Sink<'a> {
    store: Arc<dyn Store>,
    files: BTreeMap<String, (u64, u64)>,
    end: u64,
    log: &'a (dyn Fn(&str) + Sync),
    buf: Vec<u8>,
}

impl Sink<'_> {
    /// Whether a file of the archive (its path) is wanted: a disc image's (by its extension), not seen before.
    fn wants(&self, path: &str) -> Option<String> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
        let wanted = !name.is_empty() && !name.starts_with("._") && EXTENSIONS.contains(&ext(&name).as_str());
        (wanted && !self.files.contains_key(&name)).then_some(name)
    }

    fn take(&mut self, name: String, from: &mut dyn Read) -> Result<()> {
        (self.log)(&format!("unpacking {name}"));
        let start = self.end;
        loop {
            let n = match from.read(&mut self.buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(anyhow!(e).context(format!("cannot unpack {name}"))),
            };
            self.store.write_at(self.end, &self.buf[..n]).with_context(|| format!("cannot unpack {name}"))?;
            self.end += n as u64;
        }
        self.files.insert(name, (start, self.end - start));
        Ok(())
    }
}

/// The disc files of the archive `archive` (`.zip`, `.7z`, ...), unpacked into `scratch` (emptied first); their
/// progress in `log`.
pub fn unpack(archive: &dyn DiscFile, scratch: Arc<dyn Store>, log: &(dyn Fn(&str) + Sync)) -> Result<Unpacked> {
    let name = archive.name();
    scratch.truncate(0)?;
    let mut sink = Sink { store: scratch.clone(), files: BTreeMap::new(), end: 0, log, buf: vec![0; 1 << 20] };
    let file = || archive.open().with_context(|| format!("cannot open {name}"));
    let tar = |from: &mut dyn Read, sink: &mut Sink| -> Result<()> {
        let mut a = tar::Archive::new(from);
        for entry in a.entries().with_context(|| format!("{name} is not a tar archive"))? {
            let mut entry = entry.with_context(|| format!("{name}: a damaged entry"))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            if let Some(n) = sink.wants(&path) {
                sink.take(n, &mut entry)?;
            }
        }
        Ok(())
    };
    // one file compressed alone: what it holds is its name without the extension (a .tar: a tarball)
    let inner = super::stem(&name).to_string();
    let single = |from: &mut dyn Read, sink: &mut Sink| -> Result<()> {
        if ext(&inner) == "tar" || ["tgz", "txz", "tzst"].contains(&ext(&name).as_str()) {
            return tar(from, sink);
        }
        let n = sink.wants(&inner).with_context(|| format!("{name} holds {inner}, which is no disc image"))?;
        sink.take(n, from)
    };
    match ext(&name).as_str() {
        "zip" => {
            let mut z =
                zip::ZipArchive::new(file()?).with_context(|| format!("{name} is not a zip archive this reads"))?;
            for i in 0..z.len() {
                let mut f = z.by_index(i).with_context(|| format!("{name}: entry {i}"))?;
                if !f.is_file() {
                    continue;
                }
                let path = f.name().map(|p| p.into_owned()).unwrap_or_default();
                if let Some(n) = sink.wants(&path) {
                    sink.take(n, &mut f)?;
                }
            }
        }
        "7z" => {
            let mut z = sevenz_rust2::ArchiveReader::new(file()?, sevenz_rust2::Password::empty())
                .map_err(|e| anyhow!("{name} is not a 7z archive this reads: {e}"))?;
            let mut failed = None;
            z.for_each_entries(|entry, reader| {
                if entry.is_directory() || !entry.has_stream() {
                    return Ok(true);
                }
                if let Some(n) = sink.wants(entry.name())
                    && let Err(e) = sink.take(n, reader)
                {
                    failed = Some(e);
                    return Ok(false);
                }
                Ok(true)
            })
            .map_err(|e| anyhow!("{name}: {e}"))?;
            if let Some(e) = failed {
                return Err(e);
            }
        }
        "tar" => tar(&mut file()?, &mut sink)?,
        "gz" | "tgz" => single(&mut flate2::read::MultiGzDecoder::new(file()?), &mut sink)?,
        "xz" | "txz" => single(&mut lzma_rust2::XzReader::new(file()?, true), &mut sink)?,
        "zst" | "tzst" => {
            let mut z =
                ruzstd::decoding::StreamingDecoder::new(file()?).map_err(|e| anyhow!("{name} is not zstd: {e}"))?;
            single(&mut z, &mut sink)?
        }
        other => bail!("{name}: .{other} is not an archive this reads"),
    }
    if sink.files.is_empty() {
        bail!("{name} holds no disc image (.cue, .bin, .chd, .iso, .ccd, .mds, .ecm or .pbp)");
    }
    scratch.flush()?;
    Ok(Unpacked { archive: name, files: sink.files, store: scratch })
}

/// A temporary file as a scratch `Store`, removed when the last reference to it goes.
pub struct TempStore {
    /// (None once dropped: the file closed before it is removed, which Windows needs)
    store: Option<crate::io::StdStore>,
    path: std::path::PathBuf,
}

impl TempStore {
    /// A new one in `dir` (made if it is not there).
    pub fn new(dir: &std::path::Path) -> Result<TempStore> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot make {}", dir.display()))?;
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let path = dir.join(format!(".ok_unpacked_{}_{nanos}.tmp", std::process::id()));
        let store = crate::io::StdStore::open(&path)?;
        Ok(TempStore { store: Some(store), path })
    }

    fn store(&self) -> Result<&crate::io::StdStore> {
        self.store.as_ref().context("the scratch file is closed")
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        drop(self.store.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Store for TempStore {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<()> {
        self.store()?.read_at(off, buf)
    }
    fn write_at(&self, off: u64, data: &[u8]) -> Result<()> {
        self.store()?.write_at(off, data)
    }
    fn size(&self) -> Result<u64> {
        self.store()?.size()
    }
    fn truncate(&self, len: u64) -> Result<()> {
        self.store()?.truncate(len)
    }
    fn flush(&self) -> Result<()> {
        self.store()?.flush()
    }
}
