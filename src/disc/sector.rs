//! Raw CD sectors (2352 bytes): the sync and header, the CD-ROM error detection (EDC) and correction (ECC) codes, as
//! ECM and the CD-ROM standard (ECMA-130) define them. What rebuilds a whole sector from an image that keeps less of
//! it: ECM's sectors, an ISO's 2048 bytes of data, a 2336-byte Mode 2 image.

/// A raw sector's size.
pub const RAW: usize = 2352;
/// Its sync pattern.
pub const SYNC: [u8; 12] = [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0];
/// The disc's first sector (LBA 0) is at 00:02:00.
const LEAD_IN: u64 = 150;

struct Tables {
    ecc_f: [u8; 256],
    ecc_b: [u8; 256],
    edc: [u32; 256],
}

const TABLES: Tables = {
    let mut t = Tables { ecc_f: [0; 256], ecc_b: [0; 256], edc: [0; 256] };
    let mut i = 0;
    while i < 256 {
        let j = ((i << 1) ^ if i & 0x80 != 0 { 0x11D } else { 0 }) as u8;
        t.ecc_f[i] = j;
        t.ecc_b[i ^ j as usize] = i as u8;
        let mut edc = i as u32;
        let mut k = 0;
        while k < 8 {
            edc = (edc >> 1) ^ if edc & 1 != 0 { 0xD801_8001 } else { 0 };
            k += 1;
        }
        t.edc[i] = edc;
        i += 1;
    }
    t
};

/// The EDC of `data`, from `edc`.
pub fn edc(mut edc: u32, data: &[u8]) -> u32 {
    for &b in data {
        edc = (edc >> 8) ^ TABLES.edc[((edc ^ b as u32) & 0xFF) as usize];
    }
    edc
}

/// One ECC block (P or Q) of the sector's bytes from 0x0C, written at `dest`.
fn ecc_block(
    s: &mut [u8; RAW],
    major_count: usize,
    minor_count: usize,
    major_mult: usize,
    minor_inc: usize,
    dest: usize,
) {
    let size = major_count * minor_count;
    for major in 0..major_count {
        let mut index = (major >> 1) * major_mult + (major & 1);
        let (mut a, mut b) = (0u8, 0u8);
        for _ in 0..minor_count {
            let t = s[0x0C + index];
            index += minor_inc;
            if index >= size {
                index -= size;
            }
            a ^= t;
            b ^= t;
            a = TABLES.ecc_f[a as usize];
        }
        a = TABLES.ecc_b[(TABLES.ecc_f[a as usize] ^ b) as usize];
        s[dest + major] = a;
        s[dest + major + major_count] = a ^ b;
    }
}

/// The P and Q codes; Mode 2 computes them with the header's address taken as zero.
fn ecc(s: &mut [u8; RAW], zero_address: bool) {
    let address: [u8; 4] = [s[12], s[13], s[14], s[15]];
    if zero_address {
        s[12..16].fill(0);
    }
    ecc_block(s, 86, 24, 2, 86, 0x81C);
    ecc_block(s, 52, 43, 86, 88, 0x8C8);
    if zero_address {
        s[12..16].copy_from_slice(&address);
    }
}

/// What kind of sector: ECM's types 1-3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Mode1,
    Mode2Form1,
    Mode2Form2,
}

/// Writes the sector's EDC and ECC (its sync, header and data in place).
pub fn edc_ecc(s: &mut [u8; RAW], form: Form) {
    match form {
        Form::Mode1 => {
            let e = edc(0, &s[..0x810]);
            s[0x810..0x814].copy_from_slice(&e.to_le_bytes());
            s[0x814..0x81C].fill(0);
            ecc(s, false);
        }
        Form::Mode2Form1 => {
            let e = edc(0, &s[0x10..0x818]);
            s[0x818..0x81C].copy_from_slice(&e.to_le_bytes());
            ecc(s, true);
        }
        Form::Mode2Form2 => {
            let e = edc(0, &s[0x10..0x92C]);
            s[0x92C..0x930].copy_from_slice(&e.to_le_bytes());
        }
    }
}

fn bcd(n: u64) -> u8 {
    (((n / 10) << 4) | (n % 10)) as u8
}

/// The sync and the header (address of disc sector `lba`, `mode`).
pub fn header(s: &mut [u8; RAW], lba: u64, mode: u8) {
    let at = lba + LEAD_IN;
    s[..12].copy_from_slice(&SYNC);
    s[12] = bcd(at / 4500);
    s[13] = bcd(at / 75 % 60);
    s[14] = bcd(at % 75);
    s[15] = mode;
}

/// A whole Mode 2 sector from its 2336 bytes (subheader, data, EDC, ECC).
pub fn from_mode2(s: &mut [u8; RAW], lba: u64, body: &[u8]) {
    header(s, lba, 2);
    let n = body.len().min(RAW - 16);
    s[16..16 + n].copy_from_slice(&body[..n]);
    s[16 + n..].fill(0);
}

/// A whole sector from its 2048 bytes of data, as Mode 2 Form 1 (a PlayStation disc's data sectors).
pub fn from_data(s: &mut [u8; RAW], lba: u64, data: &[u8]) {
    header(s, lba, 2);
    s[16..24].copy_from_slice(&[0, 0, 8, 0, 0, 0, 8, 0]);
    let n = data.len().min(2048);
    s[24..24 + n].copy_from_slice(&data[..n]);
    s[24 + n..].fill(0);
    edc_ecc(s, Form::Mode2Form1);
}

/// Whether `s` holds the sync pattern at its start.
pub fn has_sync(s: &[u8]) -> bool {
    s.get(..12) == Some(&SYNC[..])
}
