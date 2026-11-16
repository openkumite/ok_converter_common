//! ECM (Neill Corlett's error code modeler): a file (a raw `.bin`) with the CD sectors' sync, EDC and ECC taken out,
//! which follow from the rest. Records of bytes kept as they are (type 0) and of sectors (1: Mode 1, kept as address
//! and data; 2: Mode 2 Form 1 and 3: Form 2, kept as subheader and data, giving their 2336 bytes after the header).
//! Read anywhere: the records are indexed once (where each starts in the file and in what it decodes to), a sector
//! decoded when it is read.

use super::sector::{self, Form, RAW};
use crate::io::{DiscFile, ReadSeek};
use anyhow::{Context, Result, bail};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

#[derive(Clone, Copy)]
struct Rec {
    /// where it starts in the decoded file
    out: u64,
    /// where its payload starts in the .ecm
    inp: u64,
    kind: u8,
    count: u64,
}

/// A kind's bytes in the .ecm and in the decoded file, per item.
fn sizes(kind: u8) -> (u64, u64) {
    match kind {
        0 => (1, 1),
        1 => (3 + 2048, 2352),
        2 => (0x804, 2336),
        _ => (0x918, 2336),
    }
}

/// An `.ecm`, decoded: the file it holds.
pub struct Ecm {
    file: Arc<dyn DiscFile>,
    name: String,
    recs: Arc<Vec<Rec>>,
    len: u64,
}

impl Ecm {
    pub fn open(file: Arc<dyn DiscFile>) -> Result<Ecm> {
        let full = file.name();
        let name = full.strip_suffix(".ecm").or_else(|| full.strip_suffix(".ECM")).unwrap_or(&full).to_string();
        let mut r = std::io::BufReader::with_capacity(1 << 20, file.open()?);
        let mut magic = [0; 4];
        r.read_exact(&mut magic).with_context(|| format!("cannot read {full}"))?;
        if &magic != b"ECM\0" {
            bail!("{full} is not an ECM file");
        }
        let byte = |r: &mut dyn Read| -> Result<u8> {
            let mut b = [0];
            r.read_exact(&mut b).with_context(|| format!("{full} ends before its end mark"))?;
            Ok(b[0])
        };
        let (mut inp, mut out) = (4u64, 0u64);
        let mut recs = Vec::new();
        loop {
            let mut c = byte(&mut r)?;
            inp += 1;
            let kind = c & 3;
            let mut num = ((c >> 2) & 0x1F) as u64;
            let mut bits = 5;
            while c & 0x80 != 0 {
                c = byte(&mut r)?;
                inp += 1;
                if bits > 31 {
                    bail!("{full} is damaged (a record's count runs on)");
                }
                num |= ((c & 0x7F) as u64) << bits;
                bits += 7;
            }
            if num == 0xFFFF_FFFF {
                break;
            }
            let count = num + 1;
            let (isz, osz) = sizes(kind);
            recs.push(Rec { out, inp, kind, count });
            let skip = isz * count;
            let skipped = std::io::copy(&mut (&mut r).take(skip), &mut std::io::sink())?;
            if skipped != skip {
                bail!("{full} ends inside a record");
            }
            inp += skip;
            out += osz * count;
        }
        Ok(Ecm { file, name, recs: Arc::new(recs), len: out })
    }
}

impl DiscFile for Ecm {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(Reader {
            inner: self.file.open()?,
            at: None,
            recs: self.recs.clone(),
            len: self.len,
            pos: 0,
            cur: None,
            sector: Box::new([0; RAW]),
            payload: vec![0; 0x918],
        }))
    }
}

struct Reader {
    inner: Box<dyn ReadSeek>,
    /// where `inner` is, when known
    at: Option<u64>,
    recs: Arc<Vec<Rec>>,
    len: u64,
    pos: u64,
    /// the sector in `sector`: its record and number there
    cur: Option<(usize, u64)>,
    sector: Box<[u8; RAW]>,
    payload: Vec<u8>,
}

impl Reader {
    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if self.at != Some(at) {
            self.inner.seek(SeekFrom::Start(at))?;
        }
        self.inner.read_exact(buf)?;
        self.at = Some(at + buf.len() as u64);
        Ok(())
    }

    fn decode(&mut self, r: usize, k: u64) -> std::io::Result<()> {
        let rec = self.recs[r];
        let (isz, _) = sizes(rec.kind);
        let mut payload = std::mem::take(&mut self.payload);
        self.read_at(rec.inp + k * isz, &mut payload[..isz as usize])?;
        let s = &mut self.sector;
        s.fill(0);
        s[..12].copy_from_slice(&sector::SYNC);
        match rec.kind {
            1 => {
                s[0x0F] = 1;
                s[0x0C..0x0F].copy_from_slice(&payload[..3]);
                s[0x10..0x810].copy_from_slice(&payload[3..0x803]);
                sector::edc_ecc(s, Form::Mode1);
            }
            kind => {
                s[0x0F] = 2;
                let n = isz as usize;
                s[0x14..0x14 + n].copy_from_slice(&payload[..n]);
                let sub: [u8; 4] = [s[0x14], s[0x15], s[0x16], s[0x17]];
                s[0x10..0x14].copy_from_slice(&sub);
                sector::edc_ecc(s, if kind == 2 { Form::Mode2Form1 } else { Form::Mode2Form2 });
            }
        }
        self.payload = payload;
        self.cur = Some((r, k));
        Ok(())
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let r = self.recs.partition_point(|x| x.out <= self.pos) - 1;
        let rec = self.recs[r];
        let (_, osz) = sizes(rec.kind);
        let into = self.pos - rec.out;
        if rec.kind == 0 {
            let n = buf.len().min((rec.count - into) as usize);
            self.read_at(rec.inp + into, &mut buf[..n])?;
            self.pos += n as u64;
            return Ok(n);
        }
        let k = into / osz;
        if self.cur != Some((r, k)) {
            self.decode(r, k)?;
        }
        let start = if rec.kind == 1 { 0 } else { 0x10 };
        let off = (into % osz) as usize;
        let n = buf.len().min(osz as usize - off);
        buf[..n].copy_from_slice(&self.sector[start + off..start + off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = super::seek_to(to, self.pos, self.len)?;
        Ok(self.pos)
    }
}
