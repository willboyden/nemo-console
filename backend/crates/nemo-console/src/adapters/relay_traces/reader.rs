//! Reading capture files: safe opening, a bounded line reader and the small hashes that tell contents apart.
//!
//! Every open goes through the adapter's own helpers: the path must stay inside its directory once symlinks are
//! resolved, the last component is opened with `O_NOFOLLOW`, and only a regular file is accepted. No name here ever
//! comes from a request: names come from listing the configured directories.

use std::io::Read;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::adapters::relay::{FileErr, contained, open_regular};

/// An open capture file and the facts that identify it.
pub struct Opened {
    pub file: std::fs::File,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

/// Open `dir/name` read-only: contained in `dir`, not a symlink, a regular file.
pub fn open_in(dir: &Path, name: &str) -> Result<Opened, FileErr> {
    // a listed name is one path component; anything else is refused before the file system is asked
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(FileErr::perm("not a plain file name"));
    }
    let path = dir.join(name);
    if !contained(&path, dir) {
        return Err(FileErr::perm("path escapes its allowed root"));
    }
    let (file, st) = open_regular(&path, None)?;
    Ok(Opened {
        file,
        dev: st.dev(),
        ino: st.ino(),
        size: st.len(),
        mtime_ns: i128::from(st.mtime()) * 1_000_000_000 + i128::from(st.mtime_nsec()),
    })
}

/// The first 16 bytes of SHA-256, as a number: enough to tell two files' beginnings apart.
#[must_use]
pub fn hash128(bytes: &[u8]) -> u128 {
    let d = Sha256::digest(bytes);
    let mut a = [0u8; 16];
    a.copy_from_slice(&d[..16]);
    u128::from_le_bytes(a)
}

/// Up to `len` bytes at `offset` (fewer at the end of the file).
pub fn read_at(file: &std::fs::File, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        match file.read_at(&mut buf[got..], offset + got as u64) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// Position of the first `\n`, eight bytes at a time.
#[must_use]
pub fn find_newline(h: &[u8]) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const NL: u64 = LO * 0x0a;
    let mut base = 0;
    let (chunks, rest) = h.as_chunks::<8>();
    for c in chunks {
        // a byte of `w` is zero exactly where the input has a newline; the classic "has a zero byte" test finds it
        let w = u64::from_le_bytes(*c) ^ NL;
        let hit = w.wrapping_sub(LO) & !w & HI;
        if hit != 0 {
            return Some(base + (hit.trailing_zeros() / 8) as usize);
        }
        base += 8;
    }
    rest.iter().position(|&b| b == b'\n').map(|p| base + p)
}

/// One line of a stream.
pub enum Line<'a> {
    /// A line of at most the limit. `start` is its offset in the stream, `consumed` its length including the newline
    /// (when `terminated`; the last line of a file may lack one).
    Data {
        bytes: &'a [u8],
        start: u64,
        consumed: u64,
        terminated: bool,
    },
    /// A line longer than the limit: its bytes were read past, not kept.
    Oversize {
        start: u64,
        consumed: u64,
        terminated: bool,
    },
    Eof,
}

const BUF: usize = 256 * 1024;
/// A spill buffer larger than this is given back after the long line that needed it.
const KEEP_SPILL: usize = 8 * 1024 * 1024;

/// Lines of a byte stream with bounded memory: a fixed buffer, plus a spill buffer that exists only while a line
/// longer than the buffer is being read and never grows past the line limit.
pub struct LineReader<R: Read> {
    src: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
    eof: bool,
    /// Stream offset of `buf[pos]`.
    offset: u64,
    spill: Vec<u8>,
    max_line: usize,
    /// A read error met while peeking at the head with bytes already buffered (a gzip rotation cut short before its
    /// first 4 KiB): the bytes are handed out first, then every later fill reports this error.
    cut: Option<std::io::ErrorKind>,
}

impl<R: Read> LineReader<R> {
    /// `offset`: where in the stream `src` starts (only used for the offsets reported with each line).
    pub fn new(src: R, offset: u64, max_line: usize) -> Self {
        LineReader {
            src,
            buf: vec![0u8; BUF].into_boxed_slice(),
            pos: 0,
            len: 0,
            eof: false,
            offset,
            spill: Vec::new(),
            max_line: max_line.max(1),
            cut: None,
        }
    }

    pub fn get_ref(&self) -> &R {
        &self.src
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.src
    }

    fn fill(&mut self) -> std::io::Result<()> {
        if let Some(kind) = self.cut {
            return Err(kind.into());
        }
        loop {
            match self.src.read(&mut self.buf[self.len..]) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(n) => {
                    self.len += n;
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The first bytes of the stream (at most `n`, fewer when it is shorter), without consuming them. Only meaningful
    /// before the first line was taken.
    pub fn peek_head(&mut self, n: usize) -> std::io::Result<&[u8]> {
        let want = n.min(self.buf.len());
        while self.len - self.pos < want && !self.eof {
            if self.pos > 0 {
                self.buf.copy_within(self.pos..self.len, 0);
                self.len -= self.pos;
                self.pos = 0;
            }
            if let Err(e) = self.fill() {
                // a stream that ends early still has a head: what was decoded before the error
                if self.len == self.pos {
                    return Err(e);
                }
                self.cut = Some(e.kind());
                break;
            }
        }
        Ok(&self.buf[self.pos..(self.pos + want).min(self.len)])
    }

    /// Stream offset of the next unread byte.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Read and drop bytes up to and including the next newline (used to start in the middle of a file).
    pub fn skip_partial_line(&mut self) -> std::io::Result<()> {
        loop {
            if let Some(i) = find_newline(&self.buf[self.pos..self.len]) {
                self.pos += i + 1;
                self.offset += i as u64 + 1;
                return Ok(());
            }
            self.offset += (self.len - self.pos) as u64;
            self.pos = 0;
            self.len = 0;
            if self.eof {
                return Ok(());
            }
            self.fill()?;
        }
    }

    /// Drop `n` bytes of the stream without looking at them, calling `seen` with every piece.
    pub fn skip_bytes(
        &mut self,
        mut n: u64,
        mut seen: impl FnMut(u64, &[u8]),
    ) -> std::io::Result<u64> {
        let mut done = 0u64;
        while n > 0 {
            if self.pos == self.len {
                self.pos = 0;
                self.len = 0;
                if self.eof {
                    break;
                }
                self.fill()?;
                continue;
            }
            let take = usize::try_from(n)
                .unwrap_or(usize::MAX)
                .min(self.len - self.pos);
            seen(self.offset, &self.buf[self.pos..self.pos + take]);
            self.pos += take;
            self.offset += take as u64;
            n -= take as u64;
            done += take as u64;
        }
        Ok(done)
    }

    /// The next line. After an error the reader must not be used again.
    pub fn next_line(&mut self) -> std::io::Result<Line<'_>> {
        loop {
            if let Some(i) = find_newline(&self.buf[self.pos..self.len]) {
                let start = self.offset;
                let from = self.pos;
                self.pos += i + 1;
                self.offset += i as u64 + 1;
                return Ok(if i > self.max_line {
                    Line::Oversize {
                        start,
                        consumed: i as u64 + 1,
                        terminated: true,
                    }
                } else {
                    Line::Data {
                        bytes: &self.buf[from..from + i],
                        start,
                        consumed: i as u64 + 1,
                        terminated: true,
                    }
                });
            }
            if self.eof {
                if self.pos == self.len {
                    return Ok(Line::Eof);
                }
                // the last line has no newline (yet)
                let start = self.offset;
                let (from, to) = (self.pos, self.len);
                self.pos = self.len;
                self.offset += (to - from) as u64;
                return Ok(if to - from > self.max_line {
                    Line::Oversize {
                        start,
                        consumed: (to - from) as u64,
                        terminated: false,
                    }
                } else {
                    Line::Data {
                        bytes: &self.buf[from..to],
                        start,
                        consumed: (to - from) as u64,
                        terminated: false,
                    }
                });
            }
            if self.pos > 0 {
                self.buf.copy_within(self.pos..self.len, 0);
                self.len -= self.pos;
                self.pos = 0;
            }
            if self.len == self.buf.len() {
                return self.long_line();
            }
            self.fill()?;
        }
    }

    /// A line that does not fit the buffer: collect it in the spill buffer up to the limit, then only count.
    fn long_line(&mut self) -> std::io::Result<Line<'_>> {
        let start = self.offset;
        let mut total: u64 = 0;
        let mut over = false;
        self.spill.clear();
        let terminated = loop {
            let chunk = &self.buf[self.pos..self.len];
            let (take, found) = match find_newline(chunk) {
                Some(i) => (i, true),
                None => (chunk.len(), false),
            };
            if !over {
                if self.spill.len() + take > self.max_line {
                    over = true;
                    self.spill = Vec::new();
                } else {
                    self.spill.extend_from_slice(&chunk[..take]);
                }
            }
            total += take as u64;
            if found {
                self.pos += take + 1;
                total += 1;
                break true;
            }
            self.pos = 0;
            self.len = 0;
            if self.eof {
                break false;
            }
            self.fill()?;
            if self.eof && self.len == 0 {
                break false;
            }
        };
        self.offset += total;
        if over {
            return Ok(Line::Oversize {
                start,
                consumed: total,
                terminated,
            });
        }
        Ok(Line::Data {
            bytes: &self.spill,
            start,
            consumed: total,
            terminated,
        })
    }

    /// Give back a spill buffer that grew large for one line.
    pub fn release_spill(&mut self) {
        if self.spill.capacity() > KEEP_SPILL {
            self.spill = Vec::new();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream cut short before the head was complete (a gzip rotation still being written) still has a head; the
    /// lines in it are handed out, then the error.
    #[test]
    fn a_stream_cut_short_has_a_head_and_then_reports_the_cut() {
        struct Cut(Vec<u8>, bool);
        impl Read for Cut {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
                self.1 = true;
                let n = self.0.len().min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                Ok(n)
            }
        }
        let mut lr = LineReader::new(Cut(b"one\ntwo\nthr".to_vec(), false), 0, 1024);
        assert_eq!(lr.peek_head(4096).unwrap(), b"one\ntwo\nthr");
        assert!(matches!(lr.next_line().unwrap(), Line::Data { bytes, .. } if bytes == b"one"));
        assert!(matches!(lr.next_line().unwrap(), Line::Data { bytes, .. } if bytes == b"two"));
        let err = loop {
            match lr.next_line() {
                Err(e) => break e,
                Ok(Line::Eof) => panic!("the cut must be reported, not read as the end"),
                Ok(_) => {}
            }
        };
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        // nothing at all before the cut: no head, the error
        let mut empty = LineReader::new(Cut(Vec::new(), true), 0, 1024);
        assert!(empty.peek_head(4096).is_err());
    }

    fn lines(data: &[u8], max: usize, chunk: usize) -> Vec<(String, u64, u64, bool)> {
        struct Slow<'a>(&'a [u8], usize);
        impl Read for Slow<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.0.len().min(self.1).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        let mut r = LineReader::new(Slow(data, chunk), 100, max);
        let mut out = Vec::new();
        loop {
            match r.next_line().unwrap() {
                Line::Eof => break,
                Line::Data {
                    bytes,
                    start,
                    consumed,
                    terminated,
                } => out.push((
                    String::from_utf8_lossy(&bytes[..bytes.len().min(12)]).into_owned(),
                    start,
                    consumed,
                    terminated,
                )),
                Line::Oversize {
                    start,
                    consumed,
                    terminated,
                } => out.push(("<oversize>".to_owned(), start, consumed, terminated)),
            }
            r.release_spill();
        }
        out
    }

    #[test]
    fn newline_search() {
        for n in 0..40 {
            let mut v = vec![b'x'; 40];
            assert_eq!(find_newline(&v), None);
            v[n] = b'\n';
            assert_eq!(find_newline(&v), Some(n), "{n}");
            v[39] = b'\n';
            assert_eq!(find_newline(&v), Some(n));
        }
        assert_eq!(find_newline(b""), None);
        assert_eq!(
            find_newline(&[0x8a, 0x0b, 0x09, 0xff, 0x00, 0x1a, 0x2a, 0x0a]),
            Some(7)
        );
    }

    #[test]
    fn lines_with_offsets_long_lines_and_an_unterminated_tail() {
        let long = "L".repeat(BUF * 2 + 17);
        let huge = "H".repeat(BUF * 3);
        let data = format!("one\n\ntwo two\n{long}\n{huge}\nafter\ntail-without-newline");
        for chunk in [1usize << 20, 4096, 7] {
            let got = lines(data.as_bytes(), BUF * 2 + 100, chunk);
            let l = long.len() as u64;
            let h = huge.len() as u64;
            assert_eq!(
                got,
                vec![
                    ("one".to_owned(), 100, 4, true),
                    (String::new(), 104, 1, true),
                    ("two two".to_owned(), 105, 8, true),
                    ("LLLLLLLLLLLL".to_owned(), 113, l + 1, true),
                    ("<oversize>".to_owned(), 114 + l, h + 1, true),
                    ("after".to_owned(), 115 + l + h, 6, true),
                    ("tail-without".to_owned(), 121 + l + h, 20, false),
                ],
                "chunk {chunk}"
            );
        }
        // an oversize line inside the buffer, and an oversize tail
        let got = lines(b"0123456789\nab\n0123456789", 5, 4096);
        assert_eq!(
            got,
            vec![
                ("<oversize>".to_owned(), 100, 11, true),
                ("ab".to_owned(), 111, 3, true),
                ("<oversize>".to_owned(), 114, 10, false),
            ]
        );
        assert!(lines(b"", 10, 10).is_empty());
    }

    #[test]
    fn skipping_bytes_and_a_partial_first_line() {
        let data = b"aaaa\nbbbb\ncccc\n";
        let mut r = LineReader::new(&data[2..], 2, 100);
        r.skip_partial_line().unwrap();
        match r.next_line().unwrap() {
            Line::Data { bytes, start, .. } => assert_eq!((bytes, start), (&b"bbbb"[..], 5)),
            _ => panic!(),
        }
        let mut r = LineReader::new(&data[..], 0, 100);
        let mut seen = Vec::new();
        assert_eq!(
            r.skip_bytes(10, |off, b| seen.push((off, b.to_vec())))
                .unwrap(),
            10
        );
        assert_eq!(seen, vec![(0, b"aaaa\nbbbb\n".to_vec())]);
        match r.next_line().unwrap() {
            Line::Data { bytes, start, .. } => assert_eq!((bytes, start), (&b"cccc"[..], 10)),
            _ => panic!(),
        }
        assert_eq!(r.skip_bytes(99, |_, _| {}).unwrap(), 0);
        // peeking leaves the stream where it was
        let mut r = LineReader::new(&data[..], 0, 100);
        assert_eq!(r.peek_head(6).unwrap(), b"aaaa\nb");
        assert_eq!(r.peek_head(4096).unwrap(), &data[..]);
        assert_eq!(r.offset(), 0);
        match r.next_line().unwrap() {
            Line::Data { bytes, start, .. } => assert_eq!((bytes, start), (&b"aaaa"[..], 0)),
            _ => panic!(),
        }
        assert_eq!(r.offset(), 5);
    }

    #[test]
    fn opening_refuses_symlinks_directories_and_names_that_are_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.jsonl"), b"x").unwrap();
        std::fs::write(tmp.path().join("outside"), b"secret").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("outside"), dir.join("link.jsonl")).unwrap();
        std::fs::create_dir(dir.join("sub.jsonl")).unwrap();
        let o = open_in(&dir, "a.jsonl").unwrap();
        assert_eq!(o.size, 1);
        assert_eq!(read_at(&o.file, 0, 10).unwrap(), b"x");
        for bad in [
            "link.jsonl",
            "sub.jsonl",
            "../outside",
            "",
            ".",
            "..",
            "a/b",
            "missing",
        ] {
            assert!(open_in(&dir, bad).is_err(), "{bad}");
        }
        assert_ne!(hash128(b"a"), hash128(b"b"));
    }
}
