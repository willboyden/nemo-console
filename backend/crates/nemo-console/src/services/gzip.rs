//! Reading a gzip file the way Python's `gzip.GzipFile(...).read()` does (RFC 1952 members, RFC 1951 inflate, CRC32
//! and ISIZE checks, several members, zero padding between them), for the Relay adapter's `.jsonl.N.gz` ATOF rotations.
//! No compression crate is declared for the port, so the inflater is written here (after zlib's `puff.c`).
//!
//! The caller passes an output cap (the bomb guard, Python's `MAX_GZ_BYTES`) and a tail window: only the last `window`
//! decompressed bytes are kept, so memory stays bounded however large the member is.
//!
//! Written by workstream A (in its adapter); moved into core at the workstream A merge (PORTING.md A-6).

/// Why a gzip file could not be read, mapped to the Python exception that would surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GzError {
    /// `gzip.BadGzipFile` (not gzip, unknown method, CRC / length mismatch).
    BadGzip(String),
    /// `EOFError` (truncated).
    Eof,
    /// `zlib.error` (corrupt deflate data). Python does not catch this one in the Relay adapter.
    Zlib(String),
    /// More than `cap` decompressed bytes (Python raises PermissionError).
    TooLarge,
}

/// The decompressed tail: last `window` bytes, and the total decompressed size.
#[derive(Debug, Default)]
pub struct Tail {
    pub data: Vec<u8>,
    pub total: u64,
}

struct Out {
    buf: Vec<u8>,
    /// Bytes dropped from the front of `buf` so far.
    dropped: u64,
    window: usize,
    cap: u64,
    crc: u32,
    member_start: u64,
}

const HISTORY: usize = 32 * 1024;

impl Out {
    fn total(&self) -> u64 {
        self.dropped + self.buf.len() as u64
    }

    fn push(&mut self, b: u8) -> Result<(), GzError> {
        self.buf.push(b);
        self.crc = crc_update(self.crc, &[b]);
        if self.total() > self.cap {
            return Err(GzError::TooLarge);
        }
        self.trim();
        Ok(())
    }

    fn trim(&mut self) {
        let keep = self.window.max(HISTORY);
        if self.buf.len() > keep.saturating_mul(2).saturating_add(65_536) {
            let cut = self.buf.len() - keep;
            self.buf.drain(..cut);
            self.dropped += cut as u64;
        }
    }

    /// The byte `dist` positions back (1 = the last one).
    fn back(&self, dist: usize) -> Option<u8> {
        if dist == 0 || dist > self.buf.len() {
            return None;
        }
        // a back-reference may not reach before the start of the current member
        if (self.total() - dist as u64) < self.member_start {
            return None;
        }
        Some(self.buf[self.buf.len() - dist])
    }
}

static CRC_TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
    let mut t = [0u32; 256];
    for (n, slot) in t.iter_mut().enumerate() {
        let mut c = u32::try_from(n).unwrap_or(0);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    t
});

/// Update a running CRC-32 (the value before the final XOR is kept inverted, like zlib's `crc32`).
#[must_use]
pub fn crc_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl Bits<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, GzError> {
        let mut val = self.bitbuf;
        while self.bitcnt < need {
            let Some(&b) = self.data.get(self.pos) else {
                return Err(GzError::Eof);
            };
            self.pos += 1;
            val |= u32::from(b) << self.bitcnt;
            self.bitcnt += 8;
        }
        self.bitbuf = if need >= 32 { 0 } else { val >> need };
        self.bitcnt -= need;
        Ok(if need >= 32 {
            val
        } else {
            val & ((1u32 << need) - 1)
        })
    }

    fn align(&mut self) {
        self.bitbuf = 0;
        self.bitcnt = 0;
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

fn decode(s: &mut Bits<'_>, h: &Huffman) -> Result<u16, GzError> {
    let mut code: i32 = 0;
    let mut first: i32 = 0;
    let mut index: i32 = 0;
    for len in 1..16 {
        code |= i32::try_from(s.bits(1)?).unwrap_or(0);
        let count = i32::from(h.count[len]);
        if code - count < first {
            let i = usize::try_from(index + (code - first)).unwrap_or(0);
            return h
                .symbol
                .get(i)
                .copied()
                .ok_or_else(|| GzError::Zlib("invalid code".into()));
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    Err(GzError::Zlib("invalid code -- missing end-of-block".into()))
}

/// Build a canonical Huffman table; returns the "left" count (0 = complete, >0 = incomplete, <0 = over-subscribed).
fn construct(lengths: &[u16]) -> (Huffman, i32) {
    let mut h = Huffman {
        count: [0; 16],
        symbol: vec![0; lengths.len()],
    };
    for &l in lengths {
        h.count[usize::from(l)] += 1;
    }
    if usize::from(h.count[0]) == lengths.len() {
        return (h, 0);
    }
    let mut left: i32 = 1;
    for len in 1..16 {
        left <<= 1;
        left -= i32::from(h.count[len]);
        if left < 0 {
            return (h, left);
        }
    }
    let mut offs = [0u16; 16];
    for len in 1..15 {
        offs[len + 1] = offs[len] + h.count[len];
    }
    for (sym, &l) in lengths.iter().enumerate() {
        if l != 0 {
            let o = &mut offs[usize::from(l)];
            h.symbol[usize::from(*o)] = u16::try_from(sym).unwrap_or(0);
            *o += 1;
        }
    }
    (h, left)
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn codes(s: &mut Bits<'_>, out: &mut Out, len: &Huffman, dist: &Huffman) -> Result<(), GzError> {
    loop {
        let sym = decode(s, len)?;
        if sym < 256 {
            out.push(u8::try_from(sym).unwrap_or(0))?;
        } else if sym == 256 {
            return Ok(());
        } else {
            let sym = usize::from(sym - 257);
            if sym >= 29 {
                return Err(GzError::Zlib("invalid literal/length code".into()));
            }
            let n = usize::from(LBASE[sym]) + s.bits(LEXT[sym])? as usize;
            let dsym = usize::from(decode(s, dist)?);
            if dsym >= 30 {
                return Err(GzError::Zlib("invalid distance code".into()));
            }
            let d = usize::from(DBASE[dsym]) + s.bits(DEXT[dsym])? as usize;
            for _ in 0..n {
                let b = out
                    .back(d)
                    .ok_or_else(|| GzError::Zlib("invalid distance too far back".into()))?;
                out.push(b)?;
            }
        }
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0u16; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let (lencode, _) = construct(&lengths);
    let (distcode, _) = construct(&[5u16; 30]);
    (lencode, distcode)
}

fn dynamic(s: &mut Bits<'_>) -> Result<(Huffman, Huffman), GzError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = s.bits(5)? as usize + 257;
    let ndist = s.bits(5)? as usize + 1;
    let ncode = s.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(GzError::Zlib("too many length or distance symbols".into()));
    }
    let mut lengths = [0u16; 320];
    for &o in ORDER.iter().take(ncode) {
        lengths[o] = u16::try_from(s.bits(3)?).unwrap_or(0);
    }
    let (lencode, err) = construct(&lengths[..19]);
    if err != 0 {
        return Err(GzError::Zlib("invalid code lengths set".into()));
    }
    let mut index = 0usize;
    while index < nlen + ndist {
        let sym = decode(s, &lencode)?;
        if sym < 16 {
            lengths[index] = sym;
            index += 1;
        } else {
            let (prev, rep) = match sym {
                16 => {
                    if index == 0 {
                        return Err(GzError::Zlib("invalid bit length repeat".into()));
                    }
                    (lengths[index - 1], 3 + s.bits(2)? as usize)
                }
                17 => (0, 3 + s.bits(3)? as usize),
                _ => (0, 11 + s.bits(7)? as usize),
            };
            if index + rep > nlen + ndist {
                return Err(GzError::Zlib("invalid bit length repeat".into()));
            }
            for _ in 0..rep {
                lengths[index] = prev;
                index += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err(GzError::Zlib("invalid code -- missing end-of-block".into()));
    }
    let (lencode, err) = construct(&lengths[..nlen]);
    if err < 0 || (err > 0 && nlen - usize::from(lencode.count[0]) != 1) {
        return Err(GzError::Zlib("invalid literal/lengths set".into()));
    }
    let (distcode, err) = construct(&lengths[nlen..nlen + ndist]);
    if err < 0 || (err > 0 && ndist - usize::from(distcode.count[0]) != 1) {
        return Err(GzError::Zlib("invalid distances set".into()));
    }
    Ok((lencode, distcode))
}

/// Inflate one raw DEFLATE stream starting at `data[pos..]`; returns the position after it.
fn inflate(data: &[u8], pos: usize, out: &mut Out) -> Result<usize, GzError> {
    let mut s = Bits {
        data,
        pos,
        bitbuf: 0,
        bitcnt: 0,
    };
    loop {
        let last = s.bits(1)?;
        match s.bits(2)? {
            0 => {
                s.align();
                let p = s.pos;
                if p + 4 > data.len() {
                    return Err(GzError::Eof);
                }
                let len = usize::from(data[p]) | (usize::from(data[p + 1]) << 8);
                let nlen = usize::from(data[p + 2]) | (usize::from(data[p + 3]) << 8);
                if len != (!nlen & 0xffff) {
                    return Err(GzError::Zlib("invalid stored block lengths".into()));
                }
                let start = p + 4;
                if start + len > data.len() {
                    return Err(GzError::Eof);
                }
                for &b in &data[start..start + len] {
                    out.push(b)?;
                }
                s.pos = start + len;
            }
            1 => {
                let (l, d) = fixed_tables();
                codes(&mut s, out, &l, &d)?;
            }
            2 => {
                let (l, d) = dynamic(&mut s)?;
                codes(&mut s, out, &l, &d)?;
            }
            _ => return Err(GzError::Zlib("invalid block type".into())),
        }
        if last == 1 {
            return Ok(s.pos);
        }
    }
}

fn u32_le(data: &[u8], p: usize) -> Option<u32> {
    let b = data.get(p..p + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Decompress a whole gzip file (all members), keeping the last `window` bytes; error past `cap` output bytes.
pub fn read_tail(data: &[u8], window: usize, cap: u64) -> Result<Tail, GzError> {
    let mut out = Out {
        buf: Vec::new(),
        dropped: 0,
        window,
        cap,
        crc: 0,
        member_start: 0,
    };
    let mut pos = 0usize;
    let mut first = true;
    loop {
        // between members: skip zero padding (Python's _GzipReader)
        if !first {
            while data.get(pos) == Some(&0) {
                pos += 1;
            }
        }
        if pos >= data.len() {
            break;
        }
        let magic = &data[pos..data.len().min(pos + 2)];
        if magic.len() < 2 {
            return Err(GzError::Eof);
        }
        if magic != [0x1f, 0x8b] {
            return Err(GzError::BadGzip(format!(
                "Not a gzipped file ({})",
                bytes_repr(magic)
            )));
        }
        let hdr = data.get(pos..pos + 10).ok_or(GzError::Eof)?;
        if hdr[2] != 8 {
            return Err(GzError::BadGzip("Unknown compression method".into()));
        }
        let flag = hdr[3];
        let mut p = pos + 10;
        if flag & 4 != 0 {
            let x = data.get(p..p + 2).ok_or(GzError::Eof)?;
            p += 2 + (usize::from(x[0]) | (usize::from(x[1]) << 8));
            if p > data.len() {
                return Err(GzError::Eof);
            }
        }
        for bit in [8u8, 16] {
            if flag & bit != 0 {
                loop {
                    let b = *data.get(p).ok_or(GzError::Eof)?;
                    p += 1;
                    if b == 0 {
                        break;
                    }
                }
            }
        }
        if flag & 2 != 0 {
            p += 2;
            if p > data.len() {
                return Err(GzError::Eof);
            }
        }
        out.crc = 0;
        out.member_start = out.total();
        let end = inflate(data, p, &mut out)?;
        let crc = u32_le(data, end).ok_or(GzError::Eof)?;
        let isize = u32_le(data, end + 4).ok_or(GzError::Eof)?;
        if crc != out.crc {
            return Err(GzError::BadGzip(format!(
                "CRC check failed {} != {}",
                hex32(crc),
                hex32(out.crc)
            )));
        }
        let produced = out.total() - out.member_start;
        if u64::from(isize) != (produced & 0xffff_ffff) {
            return Err(GzError::BadGzip("Incorrect length of data produced".into()));
        }
        pos = end + 8;
        first = false;
    }
    let total = out.total();
    let mut buf = out.buf;
    if buf.len() > window {
        buf.drain(..buf.len() - window);
    }
    Ok(Tail { data: buf, total })
}

fn hex32(v: u32) -> String {
    format!("{v:#x}")
}

/// Python `repr(bytes)` for the magic in the error text.
fn bytes_repr(b: &[u8]) -> String {
    let mut s = String::from("b'");
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\'' => s.push_str("\\'"),
            b'\t' => s.push_str("\\t"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            0x20..=0x7e => s.push(char::from(c)),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s.push('\'');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc_update(0, b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn not_gzip_and_empty() {
        assert!(
            matches!(read_tail(b"not gzip at all", 10, 100), Err(GzError::BadGzip(m)) if m == "Not a gzipped file (b'no')")
        );
        assert_eq!(read_tail(b"", 10, 100).map(|t| t.total), Ok(0));
        assert_eq!(
            read_tail(&[0x1f, 0x8b, 8, 0, 0], 10, 100).map(|t| t.total),
            Err(GzError::Eof)
        );
    }

    #[test]
    fn stored_block_member() {
        // a gzip member with one stored block holding "abc"
        let mut m = vec![
            0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 0x01, 3, 0, 0xfc, 0xff, b'a', b'b', b'c',
        ];
        m.extend_from_slice(&crc_update(0, b"abc").to_le_bytes());
        m.extend_from_slice(&3u32.to_le_bytes());
        let two = [m.clone(), vec![0, 0], m].concat();
        let t = read_tail(&two, 4, 100).unwrap_or_default();
        assert_eq!((t.data.as_slice(), t.total), (&b"cabc"[..], 6));
        assert_eq!(
            read_tail(&two, 4, 5).map(|t| t.total),
            Err(GzError::TooLarge)
        );
    }
}
