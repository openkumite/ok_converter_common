//! The data track's files (ISO 9660), read through its raw 2352-byte sectors (a Mode 2 sector's data at byte 24).

use crate::io::{DiscFile, ReadSeek};
use anyhow::{Context, Result, bail};
use std::io::{Read, Seek, SeekFrom};

const SECTOR: u64 = 2352;
const HDR: u64 = 24;

pub struct Iso {
    f: Box<dyn ReadSeek>,
    /// Every file, in the directory walk's order: path (`DIR/NAME.EXT`), first sector, size.
    pub files: Vec<(String, u32, u32)>,
}

impl Iso {
    pub fn open(disc: &dyn DiscFile) -> Result<Iso> {
        let f = disc.open().with_context(|| format!("cannot open the disc image {}", disc.name()))?;
        let mut iso = Iso { f, files: Vec::new() };
        let pvd = iso.sector(16)?;
        let (Some(b"CD001"), Some(root)) = (pvd.get(1..6), pvd.get(156..190)) else {
            bail!("{} is not a CD image with ISO 9660 data", disc.name());
        };
        let root = root.to_vec();
        iso.walk(u32le(&root, 2), u32le(&root, 10), String::new(), 0)?;
        Ok(iso)
    }

    /// One sector's 2048 bytes of user data (short at the end of the image).
    pub fn sector(&mut self, n: u64) -> Result<Vec<u8>> {
        self.at(n * SECTOR + HDR, 2048)
    }

    /// Whole raw sectors (2352 bytes each), for XA audio and movies.
    pub fn raw(&mut self, lba: u64, count: u64) -> Result<Vec<u8>> {
        self.at(lba * SECTOR, count * SECTOR)
    }

    /// `n` bytes at a byte offset of the track (short at its end).
    pub fn at(&mut self, offset: u64, n: u64) -> Result<Vec<u8>> {
        self.f.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::with_capacity(n as usize);
        (&mut self.f).take(n).read_to_end(&mut buf)?;
        Ok(buf)
    }

    fn walk(&mut self, lba: u32, size: u32, path: String, depth: u32) -> Result<()> {
        if depth > 16 {
            bail!("the disc's directories nest too deep");
        }
        let mut data = Vec::new();
        for i in 0..size.div_ceil(2048) {
            data.extend(self.sector(lba as u64 + i as u64)?);
        }
        let mut o = 0;
        while o < data.len() {
            let l = data[o] as usize;
            if l == 0 {
                o = (o / 2048 + 1) * 2048;
                continue;
            }
            let r = data.get(o..o + l).context("a directory record runs past its directory")?;
            o += l;
            if r.len() < 34 {
                bail!("a directory record is too short");
            }
            let (ext, sz) = (u32le(r, 2), u32le(r, 10));
            let name = r.get(33..33 + r[32] as usize).context("a directory record's name runs past it")?;
            if name == b"\0" || name == b"\x01" {
                continue;
            }
            let n = String::from_utf8_lossy(name);
            let n = n.split(';').next().unwrap_or("").to_string();
            if r[25] & 2 != 0 {
                self.walk(ext, sz, format!("{path}{n}/"), depth + 1)?;
            } else {
                self.files.push((format!("{path}{n}"), ext, sz));
            }
        }
        Ok(())
    }

    /// The file's first sector and size.
    pub fn find(&self, name: &str) -> Option<(u32, u32)> {
        self.files.iter().find(|f| f.0 == name).map(|f| (f.1, f.2))
    }

    pub fn read_file(&mut self, lba: u32, size: u32) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(size as usize + 2048);
        for i in 0..size.div_ceil(2048) {
            buf.extend(self.sector(lba as u64 + i as u64)?);
        }
        buf.truncate(size as usize);
        Ok(buf)
    }
}

fn u32le(d: &[u8], o: usize) -> u32 {
    d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
