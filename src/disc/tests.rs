//! A small disc made here (a data track with ISO 9660 files and Form 2 sectors, two audio tracks with 2-second
//! pregaps), written in each format and read back: each track must come back as the split `.bin` would hold it.
//! `real_disc_formats` (ignored) does the same with a real disc.

#![allow(clippy::unwrap_used, clippy::chunks_exact_to_as_chunks)]

use super::sector::{self, Form, RAW};
use super::*;
use std::collections::BTreeMap;
use std::io::{Cursor, Write};

#[derive(Clone)]
struct Bytes(Arc<Vec<u8>>);
impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

struct Mem(String, Arc<Vec<u8>>);
impl DiscFile for Mem {
    fn name(&self) -> String {
        self.0.clone()
    }
    fn open(&self) -> Result<Box<dyn ReadSeek>> {
        Ok(Box::new(Cursor::new(Bytes(self.1.clone()))))
    }
}

#[derive(Default)]
pub(super) struct MemFolder(BTreeMap<String, Arc<Vec<u8>>>);
impl MemFolder {
    fn put(&mut self, name: &str, data: Vec<u8>) {
        self.0.insert(name.to_string(), Arc::new(data));
    }
}
impl Folder for MemFolder {
    fn files(&self) -> Vec<(String, u64)> {
        self.0.iter().map(|(n, d)| (n.clone(), d.len() as u64)).collect()
    }
    fn file(&self, name: &str) -> Option<Arc<dyn DiscFile>> {
        self.0.get(name).map(|d| Arc::new(Mem(name.to_string(), d.clone())) as Arc<dyn DiscFile>)
    }
}

pub(super) fn read_all(t: &dyn DiscFile) -> Vec<u8> {
    let mut v = Vec::new();
    t.open().unwrap().read_to_end(&mut v).unwrap();
    v
}

const FILE: &[u8] = b"hello from the test disc";
const DATA: u64 = 40;
const PREGAP: u64 = 150;

/// The test disc's tracks, raw, as split .bin files hold them (audio from its pregap).
fn disc() -> [Vec<u8>; 3] {
    let mut t1 = Vec::new();
    for lba in 0..DATA {
        let mut d = vec![0u8; 2048];
        match lba {
            16 => {
                d[0] = 1;
                d[1..6].copy_from_slice(b"CD001");
                // the root directory: sector 18, 2048 bytes
                let root = &mut d[156..190];
                root[0] = 34;
                root[2..6].copy_from_slice(&18u32.to_le_bytes());
                root[10..14].copy_from_slice(&2048u32.to_le_bytes());
                root[25] = 2;
                root[32] = 1;
            }
            18 => {
                let mut o = 0;
                for (name, at, size, flags) in
                    [(&b"\0"[..], 18u32, 2048u32, 2u8), (b"\x01", 18, 2048, 2), (b"A.TXT;1", 20, FILE.len() as u32, 0)]
                {
                    let len = (33 + name.len()).next_multiple_of(2);
                    d[o] = len as u8;
                    d[o + 2..o + 6].copy_from_slice(&at.to_le_bytes());
                    d[o + 10..o + 14].copy_from_slice(&size.to_le_bytes());
                    d[o + 25] = flags;
                    d[o + 32] = name.len() as u8;
                    d[o + 33..o + 33 + name.len()].copy_from_slice(name);
                    o += len;
                }
            }
            20 => d[..FILE.len()].copy_from_slice(FILE),
            _ => d.iter_mut().enumerate().for_each(|(i, b)| *b = (i as u64 * 7 + lba * 13) as u8),
        }
        let mut s = [0u8; RAW];
        if (25..30).contains(&lba) {
            // Form 2 (XA audio): subheader submode 0x64, 2324 bytes of data, EDC
            sector::header(&mut s, lba, 2);
            s[16..24].copy_from_slice(&[1, 1, 0x64, 4, 1, 1, 0x64, 4]);
            for (i, b) in s[24..0x92C].iter_mut().enumerate() {
                *b = (i as u64 * 3 + lba) as u8;
            }
            sector::edc_ecc(&mut s, Form::Mode2Form2);
        } else {
            sector::from_data(&mut s, lba, &d);
        }
        t1.extend_from_slice(&s);
    }
    let audio = |n: u64, seed: u64| -> Vec<u8> {
        let mut a = vec![0u8; ((PREGAP + n) * 2352) as usize];
        for (i, b) in a[(PREGAP * 2352) as usize..].iter_mut().enumerate() {
            *b = (i as u64 * seed % 251) as u8;
        }
        a
    };
    [t1, audio(120, 5), audio(80, 11)]
}

fn msf(n: u64) -> String {
    format!("{:02}:{:02}:{:02}", n / 4500, n / 75 % 60, n % 75)
}

fn check(disc_: &Disc, want: &[Vec<u8>; 3], whole: bool) {
    assert_eq!(disc_.tracks.len(), 3, "{}", disc_.summary());
    for (k, w) in want.iter().enumerate() {
        let t = disc_.track(k as u8 + 1).unwrap();
        assert_eq!(t.audio, k > 0);
        let got = read_all(t);
        if k == 0 && !whole {
            // an ISO: the Form 1 sectors come back whole
            assert!(!t.whole_sectors());
            assert_eq!(got.len(), w.len());
            for lba in (0..DATA as usize).filter(|l| !(25..30).contains(l)) {
                assert_eq!(&got[lba * RAW..(lba + 1) * RAW], &w[lba * RAW..(lba + 1) * RAW], "sector {lba}");
            }
        } else {
            assert!(t.whole_sectors());
            assert_eq!(got.len(), w.len(), "track {} of {}", k + 1, disc_.format);
            assert!(got == *w, "track {} of {}", k + 1, disc_.format);
        }
    }
    let data = disc_.data().unwrap();
    let mut iso = Iso::open(data).unwrap();
    let (lba, size) = iso.find("A.TXT").unwrap();
    assert_eq!(iso.read_file(lba, size).unwrap(), FILE);
}

fn split_cue(name: &str) -> String {
    let mut cue = format!("FILE \"{name} (Track 1).bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n");
    for n in [2, 3] {
        cue += &format!(
            "FILE \"{name} (Track {n}).bin\" BINARY\n  TRACK 0{n} AUDIO\n    INDEX 00 00:00:00\n    INDEX 01 00:02:00\n"
        );
    }
    cue
}

#[test]
fn edc_ecc_rebuild_a_form1_sector() {
    let d = disc();
    // a sector made from its data alone is the sector; the EDC checks
    let s: [u8; RAW] = d[0][17 * RAW..18 * RAW].try_into().unwrap();
    let e = sector::edc(0, &s[0x10..0x818]);
    assert_eq!(&s[0x818..0x81C], &e.to_le_bytes());
    let mut again = [0u8; RAW];
    sector::from_data(&mut again, 17, &s[24..24 + 2048]);
    assert_eq!(again, s);
}

#[test]
fn split_bin_and_cue() {
    let want = disc();
    let mut f = MemFolder::default();
    for (k, t) in want.iter().enumerate() {
        // (the files named with _ for spaces, as some copies are)
        f.put(&format!("Game_(Track_{}).bin", k + 1), t.clone());
    }
    f.put("Game.cue", split_cue("Game").into_bytes());
    let d = Disc::open(&f, "Game.cue").unwrap();
    assert_eq!(d.format, "cue");
    check(&d, &want, true);
    // track 1 picked: the .cue beside it is found; its names are the files'
    let d = Disc::open(&f, "Game_(Track_1).bin").unwrap();
    assert_eq!(d.format, "cue");
    assert_eq!(d.data().unwrap().name(), "Game_(Track_1).bin");
    check(&d, &want, true);
    assert_eq!(main_image(&f).as_deref(), Some("Game.cue"));
}

#[test]
fn split_bin_without_a_cue() {
    let want = disc();
    let mut f = MemFolder::default();
    for (k, t) in want.iter().enumerate() {
        f.put(&format!("Game (Track {}).bin", k + 1), t.clone());
    }
    let d = Disc::open(&f, "Game (Track 1).bin").unwrap();
    assert_eq!(d.format, "bin");
    check(&d, &want, true);
    assert_eq!(main_image(&f).as_deref(), Some("Game (Track 1).bin"));
}

/// One file of all the tracks and its .cue.
fn single(want: &[Vec<u8>; 3]) -> (Vec<u8>, String) {
    let bin = want.concat();
    let (l1, l2) = (want[0].len() as u64 / 2352, want[1].len() as u64 / 2352);
    let cue = format!(
        "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 {}\n    INDEX 01 {}\n  TRACK 03 AUDIO\n    INDEX 00 {}\n    INDEX 01 {}\n",
        msf(l1),
        msf(l1 + PREGAP),
        msf(l1 + l2),
        msf(l1 + l2 + PREGAP)
    );
    (bin, cue)
}

#[test]
fn one_bin_and_cue() {
    let want = disc();
    let (bin, cue) = single(&want);
    let mut f = MemFolder::default();
    f.put("game.bin", bin);
    f.put("game.cue", cue.into_bytes());
    check(&Disc::open(&f, "game.cue").unwrap(), &want, true);
    check(&Disc::open(&f, "game.bin").unwrap(), &want, true);
}

#[test]
fn cue_with_wave_motorola_and_pregap() {
    let want = disc();
    let mut f = MemFolder::default();
    f.put("t1.bin", want[0].clone());
    // track 2: a .wav from INDEX 00; track 3: big-endian samples, its pregap not kept (PREGAP)
    let pcm = &want[1];
    let mut wav = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x02\0\x44\xac\0\0\x10\xb1\x02\0\x04\0\x10\0data".to_vec();
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    f.put("t2.wav", wav);
    let mut be = want[2][(PREGAP * 2352) as usize..].to_vec();
    be.chunks_exact_mut(2).for_each(|p| p.swap(0, 1));
    f.put("t3.raw", be);
    let cue = "FILE t1.bin BINARY\n TRACK 1 MODE2/2352\n  INDEX 1 00:00:00\nFILE \"t2.wav\" WAVE\n TRACK 2 AUDIO\n  INDEX 0 00:00:00\n  INDEX 1 00:02:00\nFILE \"t3.raw\" MOTOROLA\n TRACK 3 AUDIO\n  PREGAP 00:02:00\n  INDEX 1 00:00:00\n";
    f.put("x.cue", cue.as_bytes().to_vec());
    check(&Disc::open(&f, "x.cue").unwrap(), &want, true);
}

#[test]
fn iso() {
    let want = disc();
    let iso: Vec<u8> = want[0].chunks_exact(RAW).flat_map(|s| s[24..24 + 2048].to_vec()).collect();
    let mut f = MemFolder::default();
    f.put("game.iso", iso);
    let d = Disc::open(&f, "game.iso").unwrap();
    assert_eq!((d.format, d.tracks.len()), ("iso", 1));
    let t = d.data().unwrap();
    assert!(!t.whole_sectors());
    let got = read_all(t);
    for lba in (0..DATA as usize).filter(|l| !(25..30).contains(l)) {
        assert_eq!(&got[lba * RAW..(lba + 1) * RAW], &want[0][lba * RAW..(lba + 1) * RAW], "sector {lba}");
    }
    let mut iso = Iso::open(t).unwrap();
    let (lba, size) = iso.find("A.TXT").unwrap();
    assert_eq!(iso.read_file(lba, size).unwrap(), FILE);
}

#[test]
fn mode2_2336_and_2448_images() {
    let want = disc();
    let mut f = MemFolder::default();
    f.put("m2.bin", want[0].chunks_exact(RAW).flat_map(|s| s[16..].to_vec()).collect());
    f.put("sub.img", want[0].chunks_exact(RAW).flat_map(|s| [s, &[0xAA; 96][..]].concat()).collect());
    for name in ["m2.bin", "sub.img"] {
        let d = Disc::open(&f, name).unwrap();
        let t = d.data().unwrap();
        assert!(t.whole_sectors());
        assert!(read_all(t) == want[0], "{name}");
    }
}

#[test]
fn clonecd() {
    let want = disc();
    let mut f = MemFolder::default();
    f.put("game.img", want.concat());
    let (l1, l2) = (want[0].len() as u64 / 2352, want[1].len() as u64 / 2352);
    let ccd = format!(
        "[CloneCD]\nVersion=3\n[Disc]\nTocEntries=6\n[TRACK 1]\nMODE=2\nINDEX 1=0\n[TRACK 2]\nMODE=0\nINDEX 0={l1}\nINDEX 1={}\n[TRACK 3]\nMODE=0\nINDEX 0={}\nINDEX 1={}\n",
        l1 + PREGAP,
        l1 + l2,
        l1 + l2 + PREGAP
    );
    f.put("game.ccd", ccd.into_bytes());
    check(&Disc::open(&f, "game.ccd").unwrap(), &want, true);
    check(&Disc::open(&f, "game.img").unwrap(), &want, true);
}

/// An Alcohol .mds for an .mdf of the tracks (their pregaps not kept, as libmirage reads them).
pub(super) fn mds_for(tracks: &[(bool, u64, u64, u64)]) -> Vec<u8> {
    // tracks: audio, LBA of INDEX 01, sectors kept, pregap
    let n = tracks.len();
    let blocks_at = 0x58 + 24;
    let extra_at = blocks_at + 80 * (n + 3);
    let footer_at = extra_at + 8 * n;
    let name_at = footer_at + 16;
    let mut d = vec![0u8; name_at + 6];
    d[..16].copy_from_slice(b"MEDIA DESCRIPTOR");
    d[16] = 1;
    d[0x14..0x16].copy_from_slice(&1u16.to_le_bytes());
    d[0x50..0x54].copy_from_slice(&0x58u32.to_le_bytes());
    let s = 0x58;
    d[s + 10] = (n + 3) as u8;
    d[s + 11] = 3;
    d[s + 12..s + 14].copy_from_slice(&1u16.to_le_bytes());
    d[s + 14..s + 16].copy_from_slice(&(n as u16).to_le_bytes());
    d[s + 20..s + 24].copy_from_slice(&(blocks_at as u32).to_le_bytes());
    for (k, p) in [0xA0u8, 0xA1, 0xA2].into_iter().enumerate() {
        d[blocks_at + 80 * k + 4] = p;
    }
    let mut offset = 0u64;
    for (k, &(audio, lba, kept, pregap)) in tracks.iter().enumerate() {
        let b = blocks_at + 80 * (k + 3);
        d[b] = if audio { 0xA9 } else { 0xAB };
        d[b + 4] = k as u8 + 1;
        d[b + 12..b + 16].copy_from_slice(&((extra_at + 8 * k) as u32).to_le_bytes());
        d[b + 16..b + 18].copy_from_slice(&2352u16.to_le_bytes());
        d[b + 36..b + 40].copy_from_slice(&(lba as u32).to_le_bytes());
        d[b + 40..b + 48].copy_from_slice(&offset.to_le_bytes());
        d[b + 48..b + 52].copy_from_slice(&1u32.to_le_bytes());
        d[b + 52..b + 56].copy_from_slice(&(footer_at as u32).to_le_bytes());
        let e = extra_at + 8 * k;
        d[e..e + 4].copy_from_slice(&(pregap as u32).to_le_bytes());
        d[e + 4..e + 8].copy_from_slice(&(kept as u32).to_le_bytes());
        offset += kept * 2352;
    }
    d[footer_at..footer_at + 4].copy_from_slice(&(name_at as u32).to_le_bytes());
    d[name_at..name_at + 5].copy_from_slice(b"*.mdf");
    d
}

#[test]
fn alcohol() {
    let want = disc();
    let pg = (PREGAP * 2352) as usize;
    let mut f = MemFolder::default();
    f.put("game.mdf", [&want[0][..], &want[1][pg..], &want[2][pg..]].concat());
    let (l1, l2, l3) =
        (want[0].len() as u64 / 2352, want[1].len() as u64 / 2352 - PREGAP, want[2].len() as u64 / 2352 - PREGAP);
    f.put(
        "game.mds",
        mds_for(&[(false, 0, l1, 0), (true, l1 + PREGAP, l2, PREGAP), (true, l1 + PREGAP + l2 + PREGAP, l3, PREGAP)]),
    );
    check(&Disc::open(&f, "game.mds").unwrap(), &want, true);
    check(&Disc::open(&f, "game.mdf").unwrap(), &want, true);
}

/// ECM-encodes `raw` (a raw .bin) as ecm.c does: a Mode 2 sector's sync and header kept as bytes, its rest as a
/// Form 1 or Form 2 record when its EDC/ECC are what they would be rebuilt.
pub(super) fn ecm_encode(raw: &[u8]) -> Vec<u8> {
    let mut recs: Vec<(u8, Vec<u8>, u64)> = Vec::new();
    let mut push = |kind: u8, payload: &[u8], items: u64| match recs.last_mut() {
        Some(r) if r.0 == kind => {
            r.1.extend_from_slice(payload);
            r.2 += items;
        }
        _ => recs.push((kind, payload.to_vec(), items)),
    };
    for s in raw.chunks(RAW) {
        let Ok(s) = <&[u8; RAW]>::try_from(s) else {
            push(0, s, s.len() as u64);
            continue;
        };
        let rebuilt = |form: Form| {
            let mut t = [0u8; RAW];
            t[0x14..].copy_from_slice(&s[0x14..]);
            t[0x0F] = 2;
            let sub: [u8; 4] = [t[0x14], t[0x15], t[0x16], t[0x17]];
            t[0x10..0x14].copy_from_slice(&sub);
            sector::edc_ecc(&mut t, form);
            t[0x10..] == s[0x10..]
        };
        if sector::has_sync(s) && s[15] == 1 {
            let mut t = *s;
            sector::edc_ecc(&mut t, Form::Mode1);
            if t == *s {
                push(1, &[&s[12..15], &s[16..0x810]].concat(), 1);
                continue;
            }
        }
        if sector::has_sync(s) && s[15] == 2 {
            push(0, &s[..16], 16);
            if rebuilt(Form::Mode2Form1) {
                push(2, &s[0x14..0x818], 1);
            } else if rebuilt(Form::Mode2Form2) {
                push(3, &s[0x14..0x92C], 1);
            } else {
                push(0, &s[16..], (RAW - 16) as u64);
            }
            continue;
        }
        push(0, s, RAW as u64);
    }
    let mut out = b"ECM\0".to_vec();
    let count = |out: &mut Vec<u8>, kind: u8, n: u64| {
        let mut c = n - 1;
        out.push(((c >= 32) as u8) << 7 | ((c & 31) as u8) << 2 | kind);
        c >>= 5;
        while c != 0 {
            out.push(((c >= 128) as u8) << 7 | (c & 127) as u8);
            c >>= 7;
        }
    };
    for (kind, payload, n) in recs {
        count(&mut out, kind, n);
        out.extend_from_slice(&payload);
    }
    count(&mut out, 0, 0x1_0000_0000);
    out.extend_from_slice(&sector::edc(0, raw).to_le_bytes());
    out
}

#[test]
fn ecm() {
    let want = disc();
    let (bin, cue) = single(&want);
    let encoded = ecm_encode(&bin);
    assert!(encoded.len() < bin.len());
    let mut f = MemFolder::default();
    f.put("game.bin.ecm", encoded);
    f.put("game.cue", cue.into_bytes());
    for name in ["game.cue", "game.bin.ecm"] {
        let d = Disc::open(&f, name).unwrap();
        check(&d, &want, true);
    }
    // alone: the data track
    let mut alone = MemFolder::default();
    alone.put("t1.bin.ecm", ecm_encode(&want[0]));
    let d = Disc::open(&alone, "t1.bin.ecm").unwrap();
    assert_eq!(d.format, "ecm");
    assert!(read_all(d.data().unwrap()) == want[0]);
}

/// A popstation EBOOT.PBP of the raw disc `raw` with these tracks (audio, LBA of INDEX 01).
pub(super) fn pbp_for(raw: &[u8], tracks: &[(bool, u64)]) -> Vec<u8> {
    let bcd = |n: u64| (((n / 10) << 4) | (n % 10)) as u8;
    let pmsf = |lba: u64| {
        let a = lba + 150;
        [bcd(a / 4500), bcd(a / 75 % 60), bcd(a % 75)]
    };
    let mut pbp = vec![0u8; 0x28];
    pbp[..4].copy_from_slice(b"\0PBP");
    for k in 0..8 {
        pbp[8 + 4 * k..12 + 4 * k].copy_from_slice(&0x28u32.to_le_bytes());
    }
    let mut psar = vec![0u8; 0x100000];
    psar[..12].copy_from_slice(b"PSISOIMG0000");
    psar[0x400..0x40B].copy_from_slice(b"_SLUS_00000");
    let sectors = raw.len() as u64 / 2352;
    let mut toc: Vec<[u8; 10]> =
        vec![[0x41, 0, 0xA0, 0, 0, 0, 0, 1, 0x20, 0], [0x01, 0, 0xA1, 0, 0, 0, 0, bcd(tracks.len() as u64), 0, 0], {
            let m = pmsf(sectors);
            [0x01, 0, 0xA2, 0, 0, 0, 0, m[0], m[1], m[2]]
        }];
    for (k, &(audio, lba)) in tracks.iter().enumerate() {
        let m = pmsf(lba);
        toc.push([if audio { 0x01 } else { 0x41 }, 0, bcd(k as u64 + 1), 0, 0, 0, 0, m[0], m[1], m[2]]);
    }
    for (k, e) in toc.iter().enumerate() {
        psar[0x800 + 10 * k..0x800 + 10 * (k + 1)].copy_from_slice(e);
    }
    let mut data = Vec::new();
    for (k, chunk) in raw.chunks(0x9300).enumerate() {
        let mut block = chunk.to_vec();
        block.resize(0x9300, 0);
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&block).unwrap();
        let z = z.finish().unwrap();
        // (the first block kept as it is, as popstation does when deflate does not make it smaller)
        let stored = if k == 0 || z.len() >= block.len() { block } else { z };
        let e = 0x4000 + 32 * k;
        psar[e..e + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
        psar[e + 4..e + 8].copy_from_slice(&(stored.len() as u32).to_le_bytes());
        data.extend_from_slice(&stored);
    }
    pbp.extend_from_slice(&psar);
    pbp.extend_from_slice(&data);
    pbp
}

#[test]
fn pbp() {
    let want = disc();
    let (l1, l2) = (want[0].len() as u64 / 2352, want[1].len() as u64 / 2352);
    let mut f = MemFolder::default();
    f.put("EBOOT.PBP", pbp_for(&want.concat(), &[(false, 0), (true, l1 + PREGAP), (true, l1 + l2 + PREGAP)]));
    let d = Disc::open(&f, "EBOOT.PBP").unwrap();
    assert_eq!(d.format, "pbp");
    check(&d, &want, true);
}

#[test]
fn not_an_image() {
    let mut f = MemFolder::default();
    f.put("notes.bin", vec![7; 100_000]);
    f.put("only.cue", b"FILE \"missing.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n".to_vec());
    let e = |n: &str| Disc::open(&f, n).err().map(|e| format!("{e:#}")).unwrap_or_default();
    assert!(e("notes.bin").contains("not a disc image"), "{}", e("notes.bin"));
    assert!(e("only.cue").contains("not beside it"), "{}", e("only.cue"));
    let mut g = MemFolder::default();
    let mut pbp = vec![0u8; 0x200];
    pbp[..4].copy_from_slice(b"\0PBP");
    pbp[0x24] = 0x28;
    pbp[0x28..0x2C].copy_from_slice(b"\0PGD");
    g.put("EBOOT.PBP", pbp);
    assert!(Disc::open(&g, "EBOOT.PBP").err().map(|e| format!("{e:#}")).unwrap_or_default().contains("encrypted"));
}

/// The disc at `OK_DISC_CUE` (a .cue of split .bin files, as Redump's), written in each format into `OK_DISC_OUT`
/// (an empty folder; chdman makes the .chd when it is on the PATH) and read back: each track as the .bin is, the ISO's
/// data where it has it. `OK_DISC_CUE=... OK_DISC_OUT=... cargo test --release --features disc real_disc -- --ignored`
#[test]
#[ignore]
fn real_disc_formats() {
    use std::path::PathBuf;
    let cue = PathBuf::from(std::env::var("OK_DISC_CUE").unwrap());
    let out = PathBuf::from(std::env::var("OK_DISC_OUT").unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let disc = Disc::open_path(&cue).unwrap();
    println!("{}", disc.summary());
    let tracks: Vec<Vec<u8>> = disc.tracks.iter().map(|t| read_all(t)).collect();
    let want: Vec<u128> = tracks.iter().map(|t| fnv(t)).collect();
    let audio_pregap = PREGAP;
    let lens: Vec<u64> = tracks.iter().map(|t| t.len() as u64 / 2352).collect();
    let starts: Vec<u64> = lens
        .iter()
        .scan(0, |a, l| {
            let s = *a;
            *a += l;
            Some(s)
        })
        .collect();
    let stem = "disc";
    let write = |name: &str, data: &[u8]| std::fs::write(out.join(name), data).unwrap();
    let all = tracks.concat();
    // one .bin and .cue; .ecm of it; CloneCD; Alcohol; PBP; ISO
    let mut sheet = format!("FILE \"{stem}.bin\" BINARY\n");
    for (k, t) in disc.tracks.iter().enumerate() {
        sheet += &if t.audio {
            format!(
                "  TRACK {:02} AUDIO\n    INDEX 00 {}\n    INDEX 01 {}\n",
                k + 1,
                msf(starts[k]),
                msf(starts[k] + audio_pregap)
            )
        } else {
            format!("  TRACK {:02} MODE2/2352\n    INDEX 01 {}\n", k + 1, msf(starts[k]))
        };
    }
    write(&format!("{stem}.cue"), sheet.as_bytes());
    write(&format!("{stem}.bin"), &all);
    write(&format!("{stem}e.bin.ecm"), &ecm_encode(&all));
    write(&format!("{stem}e.cue"), sheet.replace(&format!("{stem}.bin"), &format!("{stem}e.bin")).as_bytes());
    write(&format!("{stem}.img"), &all);
    let mut ccd = String::from("[CloneCD]\nVersion=3\n");
    for (k, t) in disc.tracks.iter().enumerate() {
        ccd += &if t.audio {
            format!("[TRACK {}]\nMODE=0\nINDEX 0={}\nINDEX 1={}\n", k + 1, starts[k], starts[k] + audio_pregap)
        } else {
            format!("[TRACK {}]\nMODE=2\nINDEX 1={}\n", k + 1, starts[k])
        };
    }
    write(&format!("{stem}.ccd"), ccd.as_bytes());
    let pg = (audio_pregap * 2352) as usize;
    let mdf: Vec<u8> =
        disc.tracks.iter().zip(&tracks).flat_map(|(t, d)| if t.audio { d[pg..].to_vec() } else { d.clone() }).collect();
    write(&format!("{stem}.mdf"), &mdf);
    let mds: Vec<(bool, u64, u64, u64)> = disc
        .tracks
        .iter()
        .enumerate()
        .map(|(k, t)| {
            if t.audio {
                (true, starts[k] + audio_pregap, lens[k] - audio_pregap, audio_pregap)
            } else {
                (false, starts[k], lens[k], 0)
            }
        })
        .collect();
    write(&format!("{stem}.mds"), &mds_for(&mds));
    let toc: Vec<(bool, u64)> = disc
        .tracks
        .iter()
        .enumerate()
        .map(|(k, t)| (t.audio, if t.audio { starts[k] + audio_pregap } else { starts[k] }))
        .collect();
    write("EBOOT.PBP", &pbp_for(&all, &toc));
    let iso: Vec<u8> = tracks[0].chunks_exact(RAW).flat_map(|s| s[24..24 + 2048].to_vec()).collect();
    write(&format!("{stem}.iso"), &iso);
    let mut images = vec![
        format!("{stem}.cue"),
        format!("{stem}e.cue"),
        format!("{stem}.ccd"),
        format!("{stem}.mds"),
        "EBOOT.PBP".into(),
    ];
    let chd = out.join(format!("{stem}.chd"));
    let made = std::process::Command::new("chdman")
        .args(["createcd", "-f", "-i"])
        .arg(out.join(format!("{stem}.cue")))
        .arg("-o")
        .arg(&chd)
        .status();
    if made.is_ok_and(|s| s.success()) {
        images.push(format!("{stem}.chd"));
    } else {
        println!("no chdman: the .chd is not tried");
    }
    for image in &images {
        let d = Disc::open_path(&out.join(image)).unwrap();
        println!("{}", d.summary());
        let got: Vec<u128> = d.tracks.iter().map(|t| fnv(&read_all(t))).collect();
        assert_eq!(got, want, "{image}");
    }
    let d = Disc::open_path(&out.join(format!("{stem}.iso"))).unwrap();
    let mut a = Iso::open(disc.data().unwrap()).unwrap();
    let mut b = Iso::open(d.data().unwrap()).unwrap();
    assert_eq!(a.files, b.files);
    for (name, lba, size) in a.files.clone() {
        assert!(a.read_file(lba, size).unwrap() == b.read_file(lba, size).unwrap(), "{name}");
    }
}

/// FNV-1a (128 bits): the real-disc test compares tracks by it (a track is too big to keep twice).
fn fnv(d: &[u8]) -> u128 {
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for &b in d {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000000001000000000000000000013B);
    }
    h
}
