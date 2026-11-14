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
#[allow(clippy::cast_possible_truncation)]
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

    /// A tiny deflate writer for the tests: fixed Huffman codes, greedy matches from a hash of three bytes.
    struct Enc {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Enc {
        fn bits(&mut self, v: u32, n: u32) {
            self.acc |= u64::from(v) << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }

        /// A Huffman code goes most significant bit first.
        fn code(&mut self, code: u32, len: u32) {
            self.bits(code.reverse_bits() >> (32 - len), len);
        }

        fn lit(&mut self, sym: u32) {
            match sym {
                0..=143 => self.code(0x30 + sym, 8),
                144..=255 => self.code(0x190 + sym - 144, 9),
                256..=279 => self.code(sym - 256, 7),
                _ => self.code(0xc0 + sym - 280, 8),
            }
        }

        fn matched(&mut self, len: usize, dist: usize) {
            let li = LBASE.iter().rposition(|&b| usize::from(b) <= len).unwrap();
            self.lit(257 + li as u32);
            self.bits((len - usize::from(LBASE[li])) as u32, LEXT[li]);
            let di = DBASE.iter().rposition(|&b| usize::from(b) <= dist).unwrap();
            self.code(di as u32, 5);
            self.bits((dist - usize::from(DBASE[di])) as u32, DEXT[di]);
        }
    }

    fn deflate_fixed(data: &[u8]) -> Vec<u8> {
        let mut e = Enc {
            out: Vec::new(),
            acc: 0,
            n: 0,
        };
        e.bits(1, 1); // last block
        e.bits(1, 2); // fixed codes
        let mut heads: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
        let mut i = 0;
        while i < data.len() {
            let mut best = 0;
            let mut at = 0;
            if i + 3 <= data.len() {
                let key = [data[i], data[i + 1], data[i + 2]];
                if let Some(&p) = heads.get(&key)
                    && i - p <= 32768
                {
                    let mut l = 0;
                    while l < 258 && i + l < data.len() && data[p + l] == data[i + l] {
                        l += 1;
                    }
                    best = l;
                    at = p;
                }
                heads.insert(key, i);
            }
            if best >= 3 {
                e.matched(best, i - at);
                i += best;
            } else {
                e.lit(u32::from(data[i]));
                i += 1;
            }
        }
        e.lit(256);
        if e.n > 0 {
            e.bits(0, 8 - e.n);
        }
        e.out
    }

    fn member(deflated: &[u8], plain: &[u8]) -> Vec<u8> {
        let mut m = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        m.extend_from_slice(deflated);
        m.extend_from_slice(&crc_update(0, plain).to_le_bytes());
        m.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        m
    }

    fn stream_all(gz: &[u8]) -> std::io::Result<Vec<u8>> {
        use std::io::Read as _;
        let mut out = Vec::new();
        GzReader::new(gz).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Deterministic text with repeats at many distances (so matches reach far back and overlap themselves).
    fn sample(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            match x % 7 {
                0 => out.extend(std::iter::repeat_n(
                    b'a' + (x % 26) as u8,
                    (x % 300) as usize + 1,
                )),
                1 if out.len() > 40 => {
                    let back = (x as usize % out.len().min(40_000)) + 1;
                    let len = (x >> 20) as usize % 400 + 3;
                    let start = out.len() - back;
                    for k in 0..len {
                        let b = out[start + k % back];
                        out.push(b);
                    }
                }
                _ => out
                    .extend_from_slice(format!("{{\"k{}\":{}}}\n", x % 97, x % 100_003).as_bytes()),
            }
        }
        out.truncate(n);
        out
    }

    #[test]
    fn crc_fast_equals_crc() {
        let data = sample(10_007, 3);
        for cut in [0, 1, 7, 8, 9, 1000, data.len()] {
            assert_eq!(
                crc_update_fast(0, &data[..cut]),
                crc_update(0, &data[..cut])
            );
        }
        assert_eq!(crc_update_fast(0, b"123456789"), 0xCBF4_3926);
        let half = crc_update_fast(0, &data[..5000]);
        assert_eq!(crc_update_fast(half, &data[5000..]), crc_update(0, &data));
    }

    #[test]
    fn streaming_reader_round_trips_fixed_blocks_with_matches() {
        for (n, seed) in [(0, 1), (1, 2), (300, 3), (70_000, 4), (400_000, 5)] {
            let plain = sample(n, seed);
            let gz = member(&deflate_fixed(&plain), &plain);
            assert_eq!(stream_all(&gz).unwrap(), plain, "n={n}");
            // the one-shot reader of this module agrees
            let t = read_tail(&gz, usize::MAX, u64::MAX).unwrap();
            assert_eq!(t.data, plain);
        }
    }

    #[test]
    fn streaming_reader_small_reads_members_padding_and_stored_blocks() {
        use std::io::Read as _;
        let a = sample(100_000, 7);
        let b = sample(50_000, 8);
        // member 2: stored blocks of at most 65535 bytes
        let mut stored = Vec::new();
        let chunks: Vec<&[u8]> = b.chunks(40_000).collect();
        for (i, ch) in chunks.iter().enumerate() {
            stored.push(u8::from(i + 1 == chunks.len()));
            stored.extend_from_slice(&(ch.len() as u16).to_le_bytes());
            stored.extend_from_slice(&(!(ch.len() as u16)).to_le_bytes());
            stored.extend_from_slice(ch);
        }
        let gz = [
            member(&deflate_fixed(&a), &a),
            vec![0, 0, 0],
            member(&stored, &b),
        ]
        .concat();
        let want = [a, b].concat();
        assert_eq!(stream_all(&gz).unwrap(), want);
        // a reader that hands out one byte at a time, read through a 7-byte buffer
        struct Drip<'a>(&'a [u8]);
        impl std::io::Read for Drip<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(1).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut r = GzReader::new(Drip(&gz));
        let mut got = Vec::new();
        let mut buf = [0u8; 7];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, want);
        assert_eq!(r.compressed_bytes(), gz.len() as u64);
    }

    #[test]
    fn streaming_reader_reports_truncation_after_the_data_it_could_decode() {
        use std::io::Read as _;
        let plain = sample(300_000, 11);
        let gz = member(&deflate_fixed(&plain), &plain);
        let cut = &gz[..gz.len() / 2];
        let mut r = GzReader::new(cut);
        let mut got = Vec::new();
        let err = r.read_to_end(&mut got).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(got.len() > 50_000 && got[..] == plain[..got.len()]);
        // a wrong CRC and a wrong length are refused; garbage is not gzip
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n - 8] ^= 1;
        assert_eq!(
            stream_all(&bad).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut bad = gz.clone();
        bad[n - 1] ^= 1;
        assert_eq!(
            stream_all(&bad).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            stream_all(b"plain text, not gzip").unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(
            stream_all(b"").unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        // a distance that points before the start of the member
        let mut e = Enc {
            out: Vec::new(),
            acc: 0,
            n: 0,
        };
        e.bits(1, 1);
        e.bits(1, 2);
        e.lit(u32::from(b'x'));
        e.matched(3, 2);
        e.lit(256);
        e.bits(0, 7);
        assert_eq!(
            stream_all(&member(&e.out, b"x")).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn streaming_reader_equals_the_one_shot_reader_on_the_committed_gzip_files() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut seen = 0;
        for rel in [
            "relay/atof_all.jsonl.gz",
            "relay/atof_head.jsonl.gz",
            "relay/atof_line2.jsonl.gz",
            "relay/atof_sample.jsonl.gz",
            "relay/two_members.gz",
            "relay_traces/atof/relay-events.jsonl.2.gz",
        ] {
            let gz = std::fs::read(dir.join(rel)).unwrap();
            let want = read_tail(&gz, usize::MAX, u64::MAX).unwrap();
            assert_eq!(stream_all(&gz).unwrap(), want.data, "{rel}");
            seen += 1;
        }
        assert_eq!(seen, 6);
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

    /// A deflate stream of many blocks: fixed-code blocks of about `block` input bytes, every third one stored instead,
    /// with matches that reach back across block boundaries (so a resumed decoder needs its window).
    fn deflate_blocks(data: &[u8], block: usize) -> Vec<u8> {
        let mut e = Enc {
            out: Vec::new(),
            acc: 0,
            n: 0,
        };
        let mut heads: std::collections::HashMap<[u8; 3], usize> = std::collections::HashMap::new();
        let mut i = 0;
        let mut k = 0usize;
        while i < data.len() {
            let end = (i + block).min(data.len());
            let last = u32::from(end == data.len());
            if k % 3 == 2 {
                // stored: header bits, pad to a byte, LEN, NLEN, the bytes
                let len = (end - i).min(65_535);
                let last = u32::from(i + len == data.len());
                e.bits(last, 1);
                e.bits(0, 2);
                if e.n > 0 {
                    e.bits(0, 8 - e.n);
                }
                e.bits(len as u32, 16);
                e.bits(!(len as u32) & 0xffff, 16);
                e.out.extend_from_slice(&data[i..i + len]);
                for j in i..(i + len).saturating_sub(2) {
                    heads.insert([data[j], data[j + 1], data[j + 2]], j);
                }
                i += len;
            } else {
                e.bits(last, 1);
                e.bits(1, 2);
                while i < end {
                    let mut best = 0;
                    let mut at = 0;
                    if i + 3 <= data.len() {
                        let key = [data[i], data[i + 1], data[i + 2]];
                        if let Some(&p) = heads.get(&key)
                            && i - p <= 32768
                        {
                            let mut l = 0;
                            while l < 258 && i + l < data.len() && data[p + l] == data[i + l] {
                                l += 1;
                            }
                            best = l;
                            at = p;
                        }
                        heads.insert(key, i);
                    }
                    if best >= 3 {
                        e.matched(best, i - at);
                        i += best;
                    } else {
                        e.lit(u32::from(data[i]));
                        i += 1;
                    }
                }
                e.lit(256);
            }
            k += 1;
        }
        if e.n > 0 {
            e.bits(0, 8 - e.n);
        }
        e.out
    }

    /// Several members (one of stored blocks only), each of many blocks, and zero padding between two of them.
    fn multi_member(total: usize) -> (Vec<u8>, Vec<u8>) {
        let plain = sample(total, 11);
        let cuts = [0, total / 5, total / 2, total * 3 / 4, total];
        let mut gz = Vec::new();
        for (n, w) in cuts.windows(2).enumerate() {
            let part = &plain[w[0]..w[1]];
            let deflated = if n == 2 {
                let mut stored = Vec::new();
                let chunks: Vec<&[u8]> = part.chunks(40_000).collect();
                for (i, ch) in chunks.iter().enumerate() {
                    stored.push(u8::from(i + 1 == chunks.len()));
                    stored.extend_from_slice(&(ch.len() as u16).to_le_bytes());
                    stored.extend_from_slice(&(!(ch.len() as u16)).to_le_bytes());
                    stored.extend_from_slice(ch);
                }
                stored
            } else {
                deflate_blocks(part, 50_000 + n * 7_001)
            };
            gz.extend_from_slice(&member(&deflated, part));
            if n == 1 {
                gz.extend_from_slice(&[0, 0, 0]);
            }
        }
        (gz, plain)
    }

    fn read_n<R: std::io::Read>(r: &mut R, n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match r.read(&mut out[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(e) => panic!("{e}"),
            }
        }
        out.truncate(got);
        out
    }

    #[test]
    fn checkpoints_give_random_access_equal_to_the_sequential_decode() {
        use std::io::Read as _;
        let (gz, plain) = multi_member(6 * 1024 * 1024 + 12_345);
        let mut seq = GzReader::with_checkpoints(&gz[..], 256 * 1024, 1000);
        let mut out = Vec::new();
        seq.read_to_end(&mut out).unwrap();
        assert_eq!(out, plain);
        let (cps, every) = seq.take_checkpoints();
        assert_eq!(every, 256 * 1024);
        assert!(cps.len() >= 20, "{}", cps.len());
        // checkpoints are in order, inside a member, with a full window except at a member's start
        for w in cps.windows(2) {
            assert!(w[0].out < w[1].out && w[0].bit_pos < w[1].bit_pos);
        }
        for c in &cps {
            assert!(c.member_start <= c.out);
            let want = (c.out - c.member_start).min(32 * 1024) as usize;
            assert_eq!(c.window.len(), want);
            assert_eq!(&c.window[..], &plain[c.out as usize - want..c.out as usize]);
        }
        // many offsets, each read through the nearest checkpoint at or before it; some reads run on into the next
        // member (which is then checked again)
        let mut x = 7u64;
        let mut offsets: Vec<usize> = (0..200)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % plain.len() as u64) as usize
            })
            .collect();
        offsets.extend(cps.iter().map(|c| c.out as usize));
        offsets.extend([0, plain.len() - 1, plain.len() / 5 - 3, plain.len() / 2 - 1]);
        for off in offsets {
            let i = cps.partition_point(|c| c.out as usize <= off);
            let want = &plain[off..(off + 70_000).min(plain.len())];
            let got = if i == 0 {
                let mut r = GzReader::new(&gz[..]);
                read_n(&mut r, off);
                read_n(&mut r, want.len())
            } else {
                let c = &cps[i - 1];
                let mut r = GzReader::resume(&gz[(c.bit_pos / 8) as usize..], c).unwrap();
                assert!((off as u64 - c.out) <= 2 * 256 * 1024 + 70_000);
                read_n(&mut r, off - c.out as usize);
                read_n(&mut r, want.len())
            };
            assert!(got == want, "offset {off}");
        }
        // a resumed reader runs to the end of the file without an error (later members are checked)
        let c = &cps[cps.len() / 3];
        let mut r = GzReader::resume(&gz[(c.bit_pos / 8) as usize..], c).unwrap();
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, &plain[c.out as usize..]);
    }

    /// The deepest read of a 12 MiB file of compressed (fixed-code) blocks: through the nearest checkpoint (1 MiB
    /// spacing) and from the start. Prints both (`--nocapture`); asserts only that both give the same bytes.
    #[test]
    #[allow(clippy::print_stdout)]
    fn deepest_read_through_a_checkpoint_timing() {
        use std::io::Read as _;
        let plain = sample(12 * 1024 * 1024, 5);
        let gz = member(&deflate_blocks(&plain, 64 * 1024), &plain);
        let mut r = GzReader::with_checkpoints(&gz[..], 1024 * 1024, 160);
        std::io::copy(&mut r, &mut std::io::sink()).unwrap();
        let (cps, _) = r.take_checkpoints();
        let off = plain.len() - 4096;
        let t0 = std::time::Instant::now();
        let c = &cps[cps.partition_point(|c| c.out as usize <= off) - 1];
        let mut r = GzReader::resume(&gz[(c.bit_pos / 8) as usize..], c).unwrap();
        read_n(&mut r, off - c.out as usize);
        let a = read_n(&mut r, 4096);
        let t_point = t0.elapsed();
        let t0 = std::time::Instant::now();
        let mut r = GzReader::new(&gz[..]);
        read_n(&mut r, off);
        let b = read_n(&mut r, 4096);
        let t_start = t0.elapsed();
        assert_eq!(a, b);
        assert_eq!(a, &plain[off..]);
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        println!(
            "12 MiB of fixed-code blocks ({} KiB compressed), deepest 4 KiB: {t_point:?} through a checkpoint, \
             {t_start:?} from the start; {} checkpoints, {} bytes",
            gz.len() / 1024,
            cps.len(),
            cps.iter().map(GzCheckpoint::memory).sum::<usize>()
        );
    }

    #[test]
    fn checkpoint_memory_is_capped_by_thinning() {
        use std::io::Read as _;
        let (gz, plain) = multi_member(3 * 1024 * 1024);
        let mut r = GzReader::with_checkpoints(&gz[..], 16 * 1024, 8);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, plain);
        let (cps, every) = r.take_checkpoints();
        assert!(cps.len() <= 8 && cps.len() >= 4, "{}", cps.len());
        assert!(every > 16 * 1024);
        let memory: usize = cps.iter().map(GzCheckpoint::memory).sum();
        assert!(memory <= 8 * (32 * 1024 + 128), "{memory}");
        // still usable
        let c = cps.last().unwrap();
        let mut r = GzReader::resume(&gz[(c.bit_pos / 8) as usize..], c).unwrap();
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, &plain[c.out as usize..]);
    }

    #[test]
    fn a_resumed_reader_on_other_bytes_fails_or_differs() {
        use std::io::Read as _;
        let (gz, plain) = multi_member(1024 * 1024);
        let mut r = GzReader::with_checkpoints(&gz[..], 64 * 1024, 100);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        let (cps, _) = r.take_checkpoints();
        let c = &cps[2];
        // a checkpoint whose window claims more than the member has is refused
        let mut bad = c.clone();
        bad.member_start = bad.out;
        assert!(GzReader::resume(&gz[(c.bit_pos / 8) as usize..], &bad).is_err());
        // other compressed bytes at the checkpoint's place: an error or other output, never the old output silently
        let mut other = gz.clone();
        let at = (c.bit_pos / 8) as usize;
        for b in &mut other[at..at + 64] {
            *b ^= 0x5a;
        }
        let res = GzReader::resume(&other[at..], c).and_then(|mut r| {
            let mut v = Vec::new();
            r.read_to_end(&mut v).map(|_| v)
        });
        if let Ok(v) = res {
            assert_ne!(v, &plain[c.out as usize..]);
        }
    }
}

// ── streaming reader (addendum 9.12: the Relay trace index) ─────────────────────────────────────────────────────────
//
// `read_tail` above needs the whole compressed file in memory and decodes one bit at a time: fine for the 64 MiB
// rotations `recent_events` accepts, not for the trace index, which reads rotations of a gigabyte and more. `GzReader`
// inflates as its caller reads: a fixed input buffer, a 32 KiB history plus one chunk of pending output, table-driven
// Huffman decoding, and a CRC folded eight bytes at a time. Memory stays the same however large the member is.

/// Compressed bytes pulled from the source per refill.
const IN_BUF: usize = 64 * 1024;
/// Decompressed bytes produced per round before the caller gets them.
const OUT_CHUNK: usize = 64 * 1024;
/// Codes of at most this many bits are decoded with one table lookup; longer ones take the bit-by-bit path.
const FAST_BITS: u32 = 10;
const FAST_SIZE: usize = 1 << FAST_BITS;

static CRC_SLICES: std::sync::LazyLock<Box<[[u32; 256]; 8]>> = std::sync::LazyLock::new(|| {
    let mut t = Box::new([[0u32; 256]; 8]);
    t[0] = *CRC_TABLE;
    for i in 0..256 {
        let mut c = t[0][i];
        for k in 1..8 {
            c = t[0][(c & 0xff) as usize] ^ (c >> 8);
            t[k][i] = c;
        }
    }
    t
});

/// [`crc_update`], eight bytes per step (the same value, about five times faster on long inputs).
#[must_use]
pub fn crc_update_fast(crc: u32, data: &[u8]) -> u32 {
    let t = &**CRC_SLICES;
    let mut c = !crc;
    let (chunks, rest) = data.as_chunks::<8>();
    for ch in chunks {
        let lo = u32::from_le_bytes([ch[0], ch[1], ch[2], ch[3]]) ^ c;
        let hi = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]);
        c = t[7][(lo & 0xff) as usize]
            ^ t[6][((lo >> 8) & 0xff) as usize]
            ^ t[5][((lo >> 16) & 0xff) as usize]
            ^ t[4][(lo >> 24) as usize]
            ^ t[3][(hi & 0xff) as usize]
            ^ t[2][((hi >> 8) & 0xff) as usize]
            ^ t[1][((hi >> 16) & 0xff) as usize]
            ^ t[0][(hi >> 24) as usize];
    }
    for &b in rest {
        c = t[0][((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// A canonical Huffman code with a one-lookup table for its short codes.
struct FastHuff {
    count: [u16; 16],
    symbol: [u16; 288],
    /// `(symbol << 4) | length` for every bit pattern whose code is at most `FAST_BITS` long; 0 = a longer code.
    fast: [u16; FAST_SIZE],
}

impl FastHuff {
    fn empty() -> Box<FastHuff> {
        Box::new(FastHuff {
            count: [0; 16],
            symbol: [0; 288],
            fast: [0; FAST_SIZE],
        })
    }

    /// Build from code lengths; returns zlib's "left" count (0 complete, > 0 incomplete, < 0 over-subscribed).
    fn build(&mut self, lengths: &[u16]) -> i32 {
        self.count = [0; 16];
        self.fast = [0; FAST_SIZE];
        for &l in lengths {
            self.count[usize::from(l & 15)] += 1;
        }
        if usize::from(self.count[0]) == lengths.len() {
            return 0;
        }
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= i32::from(self.count[len]);
            if left < 0 {
                return left;
            }
        }
        // RFC 1951 3.2.2: the first code of each length (codes of one length are consecutive, in symbol order)
        let mut offs = [0u16; 16];
        let mut next_code = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            let shorter = if len == 1 { 0 } else { self.count[len - 1] };
            code = (code + u32::from(shorter)) << 1;
            next_code[len] = code;
            if len < 15 {
                offs[len + 1] = offs[len] + self.count[len];
            }
        }
        for (sym, &l) in lengths.iter().enumerate() {
            let l = usize::from(l & 15);
            if l == 0 {
                continue;
            }
            let Ok(sym16) = u16::try_from(sym) else {
                continue;
            };
            if let Some(slot) = self.symbol.get_mut(usize::from(offs[l])) {
                *slot = sym16;
            }
            offs[l] += 1;
            let c = next_code[l];
            next_code[l] += 1;
            #[allow(clippy::cast_possible_truncation)]
            let l32 = l as u32;
            if l32 <= FAST_BITS {
                // codes travel most significant bit first inside a least-significant-bit-first stream: index by the
                // reversed code, then repeat for every value of the bits that follow it
                let rev = (c.reverse_bits() >> (32 - l32)) as usize;
                #[allow(clippy::cast_possible_truncation)]
                let entry = (sym16 << 4) | (l as u16);
                let mut i = rev;
                while i < FAST_SIZE {
                    self.fast[i] = entry;
                    i += 1 << l;
                }
            }
        }
        left
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GzState {
    Header,
    BlockHeader,
    Stored(usize),
    Codes,
    Trailer,
    BetweenMembers,
    Done,
}

/// A place where decoding can start again without reading the file from its beginning (zlib's `zran` idea): a deflate
/// block boundary inside a gzip member, with the 32 KiB of output before it that back-references may reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GzCheckpoint {
    /// Compressed bits consumed before the block: the byte `bit_pos / 8` of the file, `bit_pos % 8` bits into it.
    pub bit_pos: u64,
    /// Uncompressed offset of the block's first byte (over every member of the file).
    pub out: u64,
    /// Uncompressed offset where the block's member starts (a back-reference never reaches before it).
    pub member_start: u64,
    /// The output just before `out`: at most 32 KiB, never reaching before `member_start`.
    pub window: Box<[u8]>,
}

impl GzCheckpoint {
    /// Bytes of memory the checkpoint holds.
    #[must_use]
    pub fn memory(&self) -> usize {
        std::mem::size_of::<Self>() + self.window.len()
    }
}

/// Why a streaming read failed, carried inside the `io::Error` (kind `UnexpectedEof` for a truncated file,
/// `InvalidData` for everything else).
#[derive(Debug)]
pub struct GzStreamError(pub GzError);

impl std::fmt::Display for GzStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            GzError::BadGzip(m) => write!(f, "not a readable gzip file: {m}"),
            GzError::Eof => f.write_str("gzip file ends early"),
            GzError::Zlib(m) => write!(f, "corrupt deflate data: {m}"),
            GzError::TooLarge => f.write_str("gzip output over the cap"),
        }
    }
}

impl std::error::Error for GzStreamError {}

fn gz_io(e: GzError) -> std::io::Error {
    let kind = if e == GzError::Eof {
        std::io::ErrorKind::UnexpectedEof
    } else {
        std::io::ErrorKind::InvalidData
    };
    std::io::Error::new(kind, GzStreamError(e))
}

/// A gzip file as a byte stream (`std::io::Read`): every member, CRC and length checked, bounded memory.
///
/// A file that ends in the middle of a member (a rotation still being compressed) yields everything that could be
/// decoded and then an `UnexpectedEof` error, so a caller can keep what it got.
pub struct GzReader<R: std::io::Read> {
    src: R,
    inb: Box<[u8]>,
    in_pos: usize,
    in_len: usize,
    src_eof: bool,
    compressed: u64,
    bits: u64,
    nbits: u32,
    /// History (the last 32 KiB at least) followed by output the caller has not taken yet.
    out: Vec<u8>,
    out_pos: usize,
    /// Decompressed bytes dropped from the front of `out` so far.
    dropped: u64,
    member_start: u64,
    crc: u32,
    crc_pos: usize,
    state: GzState,
    last_block: bool,
    lit: Box<FastHuff>,
    dist: Box<FastHuff>,
    pending: Option<std::io::Error>,
    /// Check each member's CRC and length (off for the member a resumed reader starts inside).
    verify: bool,
    /// Record a checkpoint at the first block boundary at or after this output offset (`u64::MAX`: never).
    cp_next: u64,
    cp_every: u64,
    cp_cap: usize,
    checkpoints: Vec<GzCheckpoint>,
}

impl<R: std::io::Read> GzReader<R> {
    pub fn new(src: R) -> Self {
        GzReader {
            src,
            inb: vec![0u8; IN_BUF].into_boxed_slice(),
            in_pos: 0,
            in_len: 0,
            src_eof: false,
            compressed: 0,
            bits: 0,
            nbits: 0,
            out: Vec::with_capacity(HISTORY + 2 * OUT_CHUNK + 512),
            out_pos: 0,
            dropped: 0,
            member_start: 0,
            crc: 0,
            crc_pos: 0,
            state: GzState::Header,
            last_block: false,
            lit: FastHuff::empty(),
            dist: FastHuff::empty(),
            pending: None,
            verify: true,
            cp_next: u64::MAX,
            cp_every: u64::MAX,
            cp_cap: 0,
            checkpoints: Vec::new(),
        }
    }

    /// A reader that also records a checkpoint about every `every` bytes of output (at the next block boundary).
    /// At most `cap` are kept: when there would be more, every second one is dropped and the spacing doubles, so the
    /// checkpoints stay evenly spread and their memory stays under `cap` windows of 32 KiB.
    pub fn with_checkpoints(src: R, every: u64, cap: usize) -> Self {
        let mut r = GzReader::new(src);
        r.cp_every = every.max(1);
        r.cp_next = r.cp_every;
        r.cp_cap = cap.max(2);
        r
    }

    /// The checkpoints recorded so far (earliest first) and the spacing they ended with.
    pub fn take_checkpoints(&mut self) -> (Vec<GzCheckpoint>, u64) {
        (std::mem::take(&mut self.checkpoints), self.cp_every)
    }

    /// A reader that starts at `cp` instead of at the beginning: `src` must yield the file from byte `cp.bit_pos / 8`
    /// on. Its first byte of output is the one at `cp.out`. The member it starts inside is not CRC- or length-checked
    /// (its beginning is not read); later members are. The caller proves that the file is the one the checkpoint was
    /// taken from (identity and head) and that what it reads is what it expects.
    pub fn resume(src: R, cp: &GzCheckpoint) -> std::io::Result<Self> {
        let mut r = GzReader::new(src);
        if cp.window.len() > HISTORY || cp.out < cp.member_start + cp.window.len() as u64 {
            return Err(gz_io(GzError::BadGzip("invalid checkpoint".into())));
        }
        r.compressed = cp.bit_pos / 8;
        r.out.extend_from_slice(&cp.window);
        r.out_pos = r.out.len();
        r.crc_pos = r.out.len();
        r.dropped = cp.out - cp.window.len() as u64;
        r.member_start = cp.member_start;
        r.verify = false;
        r.state = GzState::BlockHeader;
        #[allow(clippy::cast_possible_truncation)]
        let skip = (cp.bit_pos % 8) as u32;
        if skip > 0 {
            r.refill()?;
            if r.nbits < skip {
                return Err(gz_io(GzError::Eof));
            }
            r.bits >>= skip;
            r.nbits -= skip;
        }
        Ok(r)
    }

    /// Compressed bits taken from the source and used so far.
    fn bit_pos(&self) -> u64 {
        (self.compressed - (self.in_len - self.in_pos) as u64) * 8 - u64::from(self.nbits)
    }

    /// At a block boundary: record a checkpoint when one is due.
    fn maybe_checkpoint(&mut self) {
        let at = self.dropped + self.out.len() as u64;
        if at < self.cp_next {
            return;
        }
        let since_member = usize::try_from(at - self.member_start).unwrap_or(usize::MAX);
        let w = self.out.len().min(HISTORY).min(since_member);
        self.checkpoints.push(GzCheckpoint {
            bit_pos: self.bit_pos(),
            out: at,
            member_start: self.member_start,
            window: self.out[self.out.len() - w..].into(),
        });
        if self.checkpoints.len() > self.cp_cap {
            // keep every second one (the 2nd, 4th, ...: about `2 * every` apart) and space the next ones alike
            let mut i = 0usize;
            self.checkpoints.retain(|_| {
                i += 1;
                i.is_multiple_of(2)
            });
            self.cp_every = self.cp_every.saturating_mul(2);
        }
        let last = self.checkpoints.last().map_or(at, |c| c.out);
        self.cp_next = last.saturating_add(self.cp_every);
    }

    /// Compressed bytes taken from the source so far (progress; a little ahead of what was decoded).
    #[must_use]
    pub fn compressed_bytes(&self) -> u64 {
        self.compressed
    }

    fn fill_input(&mut self) -> std::io::Result<()> {
        if self.src_eof {
            return Ok(());
        }
        // keep the unread tail (at most a few bytes) at the front
        let rest = self.in_len - self.in_pos;
        self.inb.copy_within(self.in_pos..self.in_len, 0);
        self.in_pos = 0;
        self.in_len = rest;
        while self.in_len < self.inb.len() {
            match self.src.read(&mut self.inb[self.in_len..]) {
                Ok(0) => {
                    self.src_eof = true;
                    break;
                }
                Ok(n) => {
                    self.in_len += n;
                    self.compressed += n as u64;
                    if self.in_len >= 4096 {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Top the bit buffer up to at least 56 bits when the input has them.
    #[inline]
    fn refill(&mut self) -> std::io::Result<()> {
        if self.nbits >= 56 {
            return Ok(());
        }
        if self.in_len - self.in_pos < 8 {
            self.fill_input()?;
        }
        if self.in_len - self.in_pos >= 8 {
            let mut a = [0u8; 8];
            a.copy_from_slice(&self.inb[self.in_pos..self.in_pos + 8]);
            self.bits |= u64::from_le_bytes(a) << self.nbits;
            let take = (63 - self.nbits) >> 3;
            self.in_pos += take as usize;
            self.nbits += take * 8;
        } else {
            while self.nbits <= 56 && self.in_pos < self.in_len {
                self.bits |= u64::from(self.inb[self.in_pos]) << self.nbits;
                self.in_pos += 1;
                self.nbits += 8;
            }
        }
        Ok(())
    }

    /// `n` (at most 32) bits, least significant first.
    #[inline]
    fn take(&mut self, n: u32) -> std::io::Result<u32> {
        if self.nbits < n {
            self.refill()?;
            if self.nbits < n {
                return Err(gz_io(GzError::Eof));
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let v = (self.bits & ((1u64 << n) - 1)) as u32;
        self.bits >>= n;
        self.nbits -= n;
        Ok(v)
    }

    fn byte(&mut self) -> std::io::Result<u8> {
        #[allow(clippy::cast_possible_truncation)]
        Ok(self.take(8)? as u8)
    }

    /// Drop the bits up to the next byte boundary (the bit buffer only ever holds whole following bytes).
    fn align(&mut self) {
        let drop = self.nbits % 8;
        self.bits >>= drop;
        self.nbits -= drop;
    }

    /// True when not a single byte is left (buffered or in the source).
    fn at_eof(&mut self) -> std::io::Result<bool> {
        if self.nbits > 0 || self.in_pos < self.in_len {
            return Ok(false);
        }
        self.fill_input()?;
        Ok(self.in_pos >= self.in_len)
    }

    fn header(&mut self) -> std::io::Result<()> {
        let m0 = self.byte()?;
        let m1 = self.byte()?;
        if [m0, m1] != [0x1f, 0x8b] {
            return Err(gz_io(GzError::BadGzip(format!(
                "Not a gzipped file ({})",
                bytes_repr(&[m0, m1])
            ))));
        }
        if self.byte()? != 8 {
            return Err(gz_io(GzError::BadGzip("Unknown compression method".into())));
        }
        let flag = self.byte()?;
        for _ in 0..6 {
            self.byte()?; // mtime, extra flags, OS
        }
        if flag & 4 != 0 {
            let n = usize::from(self.byte()?) | (usize::from(self.byte()?) << 8);
            for _ in 0..n {
                self.byte()?;
            }
        }
        for bit in [8u8, 16] {
            if flag & bit != 0 {
                while self.byte()? != 0 {}
            }
        }
        if flag & 2 != 0 {
            self.byte()?;
            self.byte()?;
        }
        self.crc = 0;
        self.crc_pos = self.out.len();
        self.member_start = self.dropped + self.out.len() as u64;
        self.verify = true;
        Ok(())
    }

    fn fold_crc(&mut self) {
        if self.crc_pos < self.out.len() {
            self.crc = crc_update_fast(self.crc, &self.out[self.crc_pos..]);
            self.crc_pos = self.out.len();
        }
    }

    fn trailer(&mut self) -> std::io::Result<()> {
        self.align();
        self.fold_crc();
        let crc = self.take(32)?;
        let isize = self.take(32)?;
        if !self.verify {
            return Ok(());
        }
        if crc != self.crc {
            return Err(gz_io(GzError::BadGzip(format!(
                "CRC check failed {} != {}",
                hex32(crc),
                hex32(self.crc)
            ))));
        }
        let produced = self.dropped + self.out.len() as u64 - self.member_start;
        if u64::from(isize) != (produced & 0xffff_ffff) {
            return Err(gz_io(GzError::BadGzip(
                "Incorrect length of data produced".into(),
            )));
        }
        Ok(())
    }

    fn block_header(&mut self) -> std::io::Result<()> {
        self.last_block = self.take(1)? == 1;
        match self.take(2)? {
            0 => {
                self.align();
                let len = self.take(16)?;
                let nlen = self.take(16)?;
                if len != (!nlen & 0xffff) {
                    return Err(gz_io(GzError::Zlib("invalid stored block lengths".into())));
                }
                self.state = GzState::Stored(len as usize);
            }
            1 => {
                let mut lengths = [0u16; 288];
                for (i, l) in lengths.iter_mut().enumerate() {
                    *l = match i {
                        144..=255 => 9,
                        256..=279 => 7,
                        _ => 8,
                    };
                }
                self.lit.build(&lengths);
                self.dist.build(&[5u16; 30]);
                self.state = GzState::Codes;
            }
            2 => {
                self.dynamic_tables()?;
                self.state = GzState::Codes;
            }
            _ => return Err(gz_io(GzError::Zlib("invalid block type".into()))),
        }
        Ok(())
    }

    fn dynamic_tables(&mut self) -> std::io::Result<()> {
        const ORDER: [usize; 19] = [
            16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
        ];
        let nlen = self.take(5)? as usize + 257;
        let ndist = self.take(5)? as usize + 1;
        let ncode = self.take(4)? as usize + 4;
        if nlen > 286 || ndist > 30 {
            return Err(gz_io(GzError::Zlib(
                "too many length or distance symbols".into(),
            )));
        }
        let mut lengths = [0u16; 320];
        for &o in ORDER.iter().take(ncode) {
            #[allow(clippy::cast_possible_truncation)]
            {
                lengths[o] = self.take(3)? as u16;
            }
        }
        // the code-length code is decoded through the literal table's storage, which is rebuilt right after
        if self.lit.build(&lengths[..19]) != 0 {
            return Err(gz_io(GzError::Zlib("invalid code lengths set".into())));
        }
        lengths = [0u16; 320];
        let mut index = 0usize;
        while index < nlen + ndist {
            let sym = self.symbol(true)?;
            if sym < 16 {
                lengths[index] = sym;
                index += 1;
            } else {
                let (prev, rep) = match sym {
                    16 => {
                        if index == 0 {
                            return Err(gz_io(GzError::Zlib("invalid bit length repeat".into())));
                        }
                        (lengths[index - 1], 3 + self.take(2)? as usize)
                    }
                    17 => (0, 3 + self.take(3)? as usize),
                    _ => (0, 11 + self.take(7)? as usize),
                };
                if index + rep > nlen + ndist {
                    return Err(gz_io(GzError::Zlib("invalid bit length repeat".into())));
                }
                for _ in 0..rep {
                    lengths[index] = prev;
                    index += 1;
                }
            }
        }
        if lengths[256] == 0 {
            return Err(gz_io(GzError::Zlib(
                "invalid code -- missing end-of-block".into(),
            )));
        }
        let left = self.lit.build(&lengths[..nlen]);
        if left < 0 || (left > 0 && nlen - usize::from(self.lit.count[0]) != 1) {
            return Err(gz_io(GzError::Zlib("invalid literal/lengths set".into())));
        }
        let left = self.dist.build(&lengths[nlen..nlen + ndist]);
        if left < 0 || (left > 0 && ndist - usize::from(self.dist.count[0]) != 1) {
            return Err(gz_io(GzError::Zlib("invalid distances set".into())));
        }
        Ok(())
    }

    /// Decode one symbol of the literal/length code (`lit` true) or the distance code.
    #[inline]
    fn symbol(&mut self, lit: bool) -> std::io::Result<u16> {
        if self.nbits < 15 {
            self.refill()?;
        }
        let h = if lit { &self.lit } else { &self.dist };
        #[allow(clippy::cast_possible_truncation)]
        let e = h.fast[(self.bits as usize) & (FAST_SIZE - 1)];
        if e != 0 {
            let len = u32::from(e & 15);
            if len > self.nbits {
                return Err(gz_io(GzError::Eof));
            }
            self.bits >>= len;
            self.nbits -= len;
            return Ok(e >> 4);
        }
        // a code longer than the table covers: walk the canonical code one bit at a time (zlib's puff)
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16u32 {
            if len > self.nbits {
                return Err(gz_io(GzError::Eof));
            }
            #[allow(clippy::cast_possible_truncation)]
            {
                code |= ((self.bits >> (len - 1)) & 1) as i32;
            }
            let count = i32::from(h.count[len as usize]);
            if code - count < first {
                let i = usize::try_from(index + (code - first)).unwrap_or(usize::MAX);
                let sym = h
                    .symbol
                    .get(i)
                    .copied()
                    .ok_or_else(|| gz_io(GzError::Zlib("invalid code".into())))?;
                self.bits >>= len;
                self.nbits -= len;
                return Ok(sym);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(gz_io(GzError::Zlib(
            "invalid code -- missing end-of-block".into(),
        )))
    }

    /// Decode symbols until the block ends (true) or `target` output bytes exist (false).
    fn codes(&mut self, target: usize) -> std::io::Result<bool> {
        while self.out.len() < target {
            let sym = self.symbol(true)?;
            if sym < 256 {
                #[allow(clippy::cast_possible_truncation)]
                self.out.push(sym as u8);
                continue;
            }
            if sym == 256 {
                return Ok(true);
            }
            let s = usize::from(sym - 257);
            if s >= 29 {
                return Err(gz_io(GzError::Zlib("invalid literal/length code".into())));
            }
            let n = usize::from(LBASE[s]) + self.take(LEXT[s])? as usize;
            let d = usize::from(self.symbol(false)?);
            if d >= 30 {
                return Err(gz_io(GzError::Zlib("invalid distance code".into())));
            }
            let dist = usize::from(DBASE[d]) + self.take(DEXT[d])? as usize;
            let len = self.out.len();
            // a back-reference may reach neither before the kept history nor before the start of this member
            if dist > len || self.dropped + ((len - dist) as u64) < self.member_start {
                return Err(gz_io(GzError::Zlib("invalid distance too far back".into())));
            }
            let start = len - dist;
            if dist >= n {
                self.out.extend_from_within(start..start + n);
            } else {
                // overlapping: the copy reads bytes it has just written (a run)
                for i in 0..n {
                    let b = self.out[start + i];
                    self.out.push(b);
                }
            }
        }
        Ok(false)
    }

    /// Produce more output (or reach the end). Called only when the caller has taken everything.
    fn fill(&mut self) -> std::io::Result<()> {
        // keep the history, drop what the caller already has
        if self.out.len() > HISTORY {
            self.fold_crc();
            let cut = self.out.len() - HISTORY;
            self.out.drain(..cut);
            self.dropped += cut as u64;
            self.crc_pos = self.out.len();
        }
        self.out_pos = self.out.len();
        let target = self.out.len() + OUT_CHUNK;
        while self.out.len() < target {
            match self.state {
                GzState::Header => {
                    self.header()?;
                    self.state = GzState::BlockHeader;
                }
                GzState::BlockHeader => {
                    if self.cp_next != u64::MAX {
                        self.maybe_checkpoint();
                    }
                    self.block_header()?;
                }
                GzState::Stored(n) => {
                    if n == 0 {
                        self.state = if self.last_block {
                            GzState::Trailer
                        } else {
                            GzState::BlockHeader
                        };
                        continue;
                    }
                    // the bit buffer holds whole bytes here (the block header aligned it)
                    let b = self.byte()?;
                    self.out.push(b);
                    let mut left = n - 1;
                    if self.nbits == 0 {
                        // the rest is copied straight from the input buffer: forget the bytes the bit buffer had
                        // looked at ahead of its count, or the next refill would mix them with other bytes
                        self.bits = 0;
                        while left > 0 && self.out.len() < target {
                            if self.in_pos == self.in_len {
                                self.fill_input()?;
                                if self.in_pos == self.in_len {
                                    return Err(gz_io(GzError::Eof));
                                }
                            }
                            let take = left
                                .min(self.in_len - self.in_pos)
                                .min(target - self.out.len());
                            self.out
                                .extend_from_slice(&self.inb[self.in_pos..self.in_pos + take]);
                            self.in_pos += take;
                            left -= take;
                        }
                    }
                    self.state = GzState::Stored(left);
                }
                GzState::Codes => {
                    if self.codes(target)? {
                        self.state = if self.last_block {
                            GzState::Trailer
                        } else {
                            GzState::BlockHeader
                        };
                    }
                }
                GzState::Trailer => {
                    self.trailer()?;
                    self.state = GzState::BetweenMembers;
                }
                GzState::BetweenMembers => {
                    // Python's reader skips zero padding between members
                    loop {
                        self.align();
                        if self.at_eof()? {
                            self.state = GzState::Done;
                            break;
                        }
                        if self.nbits == 0 {
                            self.refill()?;
                        }
                        if self.bits & 0xff != 0 {
                            self.state = GzState::Header;
                            break;
                        }
                        self.take(8)?;
                    }
                }
                GzState::Done => break,
            }
        }
        Ok(())
    }
}

impl<R: std::io::Read> std::io::Read for GzReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.out_pos == self.out.len() {
            if let Some(e) = self.pending.take() {
                return Err(e);
            }
            if self.state == GzState::Done {
                return Ok(0);
            }
            if let Err(e) = self.fill() {
                // hand over what was decoded before the failure; the error comes with the next call
                self.state = GzState::Done;
                if self.out_pos == self.out.len() {
                    return Err(e);
                }
                self.pending = Some(e);
            }
        }
        let n = buf.len().min(self.out.len() - self.out_pos);
        buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
        self.out_pos += n;
        Ok(n)
    }
}
