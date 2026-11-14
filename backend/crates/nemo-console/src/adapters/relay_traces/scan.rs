//! One pass over the capture directory: which files are new, which grew, and what to read.
//!
//! A file is identified by what it contains, not by its name or inode, because log rotation changes both: a rename
//! keeps the inode under a new name, `copytruncate` makes a copy with a new inode and empties the original, and
//! compression replaces a rotation with a `.gz` whose bytes are different but whose content is the same. The scan
//! therefore keeps a "lineage" per content: a hash of its first 4 KiB, how far it was read, and a hash of the last
//! line read (the anchor) to prove that a file which starts the same really is the same. A known content is
//! continued where the scan stopped, never read twice.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::event::{Parsed, parse_line};
use super::ids::{NO_TS, OddIds};
use super::reader::{Line, LineReader, Opened, hash128, is_stopped, open_in, read_at};
use super::rows::{Ingest, Loc, NO_ROW};
use crate::adapters::relay::{MAX_SCAN_ENTRIES, atof_rotation};
use crate::services::gzip::{GzCheckpoint, GzReader};

/// Bytes of a content's beginning that identify it.
pub const HEAD: usize = 4096;
/// Bytes of the last line read that prove a continuation.
const ANCHOR: usize = 256;
/// The live file is read at least this far back even when the scan budget is used up.
const LIVE_FLOOR: u64 = 64 * 1024 * 1024;
/// Marks kept per span of `relay_trace_max_spans` (marks are not spans, but they cost memory like them).
pub const MARKS_PER_SPAN: usize = 4;
/// How far past the scan budget one line may be read before the file is stopped.
const BUDGET_SLACK: u64 = 1024 * 1024;
/// How often a long read hands a partial result to the publisher.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub scan_bytes: u64,
    pub max_spans: usize,
    pub max_line_bytes: usize,
    pub max_atif_bytes: u64,
    /// Uncompressed bytes between two seek points of a `.gz` rotation ([`CHECKPOINT_EVERY`]; not a setting).
    pub checkpoint_every: u64,
}

/// A seek point into a gzip rotation about every 32 MiB of its content, so that reading one line of it (the content
/// route) decompresses at most about that much instead of everything before the line.
pub const CHECKPOINT_EVERY: u64 = 32 * 1024 * 1024;
/// Seek points kept per file: past this the spacing doubles (64, 128 MiB, ...). Each holds a 32 KiB window, so a
/// file's points stay under about 5 MiB however large it is.
pub const CHECKPOINT_CAP: usize = 160;

/// The seek points of one compressed file and the identity of the file they were taken from (device, inode, size,
/// modification time): points of another file are never used.
#[derive(Debug)]
pub struct GzPoints {
    pub key: (u64, u64, u64, i128),
    pub points: Vec<GzCheckpoint>,
}

impl GzPoints {
    /// Bytes of memory the points hold.
    #[must_use]
    pub fn memory(&self) -> usize {
        std::mem::size_of::<Self>() + self.points.iter().map(GzCheckpoint::memory).sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FState {
    Indexed,
    /// Being read, read only in part (scan budget), or cut short (a rotation still being compressed).
    Partial,
    /// Not read: beyond the scan budget or older than what the span limit keeps.
    Skipped,
    Error,
}

impl FState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FState::Indexed => "indexed",
            FState::Partial => "partial",
            FState::Skipped => "skipped",
            FState::Error => "error",
        }
    }
}

/// What `/traces/index` says about one file. The name is a file name, never a path.
#[derive(Clone, Debug)]
pub struct FileRow {
    pub name: String,
    pub bytes: u64,
    /// Bytes of the file on disk that the scan has been through (compressed bytes for a `.gz`).
    pub scanned_bytes: u64,
    pub compressed: bool,
    pub events: u64,
    pub skipped_lines: u64,
    pub state: FState,
    pub first_us: i64,
    pub last_us: i64,
}

/// Where a content can be read now.
#[derive(Clone, Debug)]
pub struct FileRef {
    pub name: String,
    pub gz: bool,
}

/// One content (see the module text).
#[derive(Clone, Debug)]
pub struct Lineage {
    pub head_len: u32,
    pub head_hash: u128,
    /// Uncompressed bytes from the start of the content that are dealt with (read, or left out on purpose).
    pub consumed: u64,
    /// Bytes at the front that were left out (the scan budget covered only the newest part).
    pub start_offset: u64,
    anchor_off: u64,
    anchor_len: u32,
    anchor_hash: u128,
    pub file: Option<FileRef>,
    /// Seek points into the compressed file that holds this content now (when it is a `.gz`).
    pub gz_points: Option<Arc<GzPoints>>,
    pub events: u64,
    pub skipped_lines: u64,
    pub first_us: i64,
    pub last_us: i64,
    /// The last read ended at the end of the content.
    at_eof: bool,
    stopped_by_budget: bool,
    failed: bool,
}

#[derive(Clone, Debug)]
struct Seen {
    size: u64,
    mtime_ns: i128,
    gz: bool,
    lineage: u32,
    row: FileRow,
}

struct Listed {
    name: String,
    rot: u64,
    gz: bool,
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
}

/// ATOF files of a directory, newest first (the live file, then rotations by number). A missing or unreadable
/// directory is an empty one.
fn list_atof(dir: &Path) -> Vec<Listed> {
    use std::os::unix::fs::MetadataExt;
    let Ok(it) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, e) in it.enumerate() {
        if i >= MAX_SCAN_ENTRIES {
            break;
        }
        let Ok(e) = e else { continue };
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        let Some(rot) = atof_rotation(&name) else {
            continue;
        };
        // DirEntry::metadata does not follow symlinks: a link is never a regular file here
        let Ok(md) = e.metadata() else { continue };
        if !md.file_type().is_file() {
            continue;
        }
        out.push(Listed {
            gz: name.ends_with(".gz"),
            name,
            rot,
            dev: md.dev(),
            ino: md.ino(),
            size: md.len(),
            mtime_ns: i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec()),
        });
    }
    out.sort_by(|a, b| (a.rot, &a.name).cmp(&(b.rot, &b.name)));
    out
}

/// What the scan tells its caller while it works (a partial snapshot can be published from it).
pub trait Progress {
    fn tick(&mut self, scan: &Scan);
}

impl<F: FnMut(&Scan)> Progress for F {
    fn tick(&mut self, scan: &Scan) {
        self(scan);
    }
}

/// The scan's state between passes.
pub struct Scan {
    pub ing: Ingest,
    pub limits: Limits,
    pub lineages: Vec<Lineage>,
    seen: HashMap<(u64, u64), Seen>,
    /// The files of the directory as of the current pass, newest first.
    pub files: Vec<FileRow>,
    /// Uncompressed bytes of never-seen content the scan may still read.
    budget_left: u64,
    /// Something was left out: the budget ran out, or the span limit dropped traces.
    pub truncated: bool,
    pub budget_skipped: u64,
    /// Files read only in part because the scan budget ran out (a rotation from its tail, a `.gz` stopped early).
    pub budget_cut: u64,
    pub cap_skipped: u64,
    pub evicted_spans: u64,
    pub evicted_marks: u64,
    pub file_errors: u64,
    last_progress: Instant,
}

enum End {
    Eof,
    Budget,
    Cancelled,
    /// A last line without its end (still being written): left for the next pass.
    TornTail,
}

impl Scan {
    #[must_use]
    pub fn new(limits: Limits) -> Scan {
        Scan {
            // the label and id tables grow with what was seen and are not emptied by eviction: both are bounded in
            // proportion to the span limit, in entries and in bytes
            ing: Ingest::with_caps(
                limits.max_spans.saturating_mul(4).saturating_add(100_000),
                limits.max_spans.saturating_mul(64).saturating_add(8 << 20),
                OddIds::with_caps(
                    limits
                        .max_spans
                        .saturating_mul(MARKS_PER_SPAN + 1)
                        .saturating_add(10_000),
                    limits.max_spans.saturating_mul(128).saturating_add(8 << 20),
                ),
            ),
            limits,
            lineages: Vec::new(),
            seen: HashMap::new(),
            files: Vec::new(),
            budget_left: limits.scan_bytes,
            truncated: false,
            budget_skipped: 0,
            budget_cut: 0,
            cap_skipped: 0,
            evicted_spans: 0,
            evicted_marks: 0,
            file_errors: 0,
            last_progress: Instant::now(),
        }
    }

    fn find_lineage(&self, head: &[u8]) -> Option<u32> {
        // lineages differ in how long their head was when first seen (a file shorter than 4 KiB): hash per length
        let mut hashes: Vec<(u32, u128)> = Vec::new();
        for (i, l) in self.lineages.iter().enumerate().rev() {
            let n = l.head_len as usize;
            if n == 0 || n > head.len() {
                continue;
            }
            let h = match hashes.iter().find(|(len, _)| *len == l.head_len) {
                Some((_, h)) => *h,
                None => {
                    let h = hash128(&head[..n]);
                    hashes.push((l.head_len, h));
                    h
                }
            };
            if h == l.head_hash {
                return u32::try_from(i).ok();
            }
        }
        None
    }

    fn new_lineage(&mut self, head: &[u8], start_offset: u64) -> Option<u32> {
        let ix = u32::try_from(self.lineages.len())
            .ok()
            .filter(|i| *i != NO_ROW)?;
        let head_hash = hash128(head);
        // a content that starts the same but turned out to be another one: the old lineage must not match again
        for l in &mut self.lineages {
            if l.head_len as usize == head.len() && l.head_hash == head_hash {
                l.head_len = 0;
                l.file = None;
            }
        }
        self.lineages.push(Lineage {
            head_len: u32::try_from(head.len()).unwrap_or(0),
            head_hash,
            consumed: start_offset,
            start_offset,
            anchor_off: 0,
            anchor_len: 0,
            anchor_hash: 0,
            file: None,
            gz_points: None,
            events: 0,
            skipped_lines: 0,
            first_us: NO_TS,
            last_us: NO_TS,
            at_eof: false,
            stopped_by_budget: false,
            failed: false,
        });
        Some(ix)
    }

    /// One pass over the ATOF directory. Returns false when it was cancelled.
    pub fn pass_atof(
        &mut self,
        dir: Option<&Path>,
        cancel: &AtomicBool,
        progress: &mut dyn Progress,
    ) -> bool {
        let listing = dir.map(list_atof).unwrap_or_default();
        // show every file at once: known ones as they were, new ones as "to be read"
        self.files = listing
            .iter()
            .map(|f| match self.seen.get(&(f.dev, f.ino)) {
                Some(s) => FileRow {
                    name: f.name.clone(),
                    bytes: f.size,
                    ..s.row.clone()
                },
                None => FileRow {
                    name: f.name.clone(),
                    bytes: f.size,
                    scanned_bytes: 0,
                    compressed: f.gz,
                    events: 0,
                    skipped_lines: 0,
                    state: FState::Partial,
                    first_us: NO_TS,
                    last_us: NO_TS,
                },
            })
            .collect();
        let mut seen_now: HashMap<(u64, u64), Seen> = HashMap::new();
        let mut used = vec![false; self.lineages.len()];
        let mut cancelled = false;
        for (i, f) in listing.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let key = (f.dev, f.ino);
            if let Some(s) = self.seen.get(&key)
                && s.size == f.size
                && s.mtime_ns == f.mtime_ns
                && s.gz == f.gz
            {
                // unchanged since the last pass (a rename keeps the inode): nothing to open
                let mut s = s.clone();
                s.row.name.clone_from(&f.name);
                if let Some(l) = self.lineages.get_mut(s.lineage as usize) {
                    l.file = Some(FileRef {
                        name: f.name.clone(),
                        gz: f.gz,
                    });
                    if let Some(u) = used.get_mut(s.lineage as usize) {
                        *u = true;
                    }
                }
                if let Some(slot) = self.files.get_mut(i) {
                    *slot = s.row.clone();
                }
                seen_now.insert(key, s);
                continue;
            }
            let Some(d) = dir else { continue };
            let (row, lineage, opened_key) = self.read_file(d, f, i, cancel, progress);
            if let Some(slot) = self.files.get_mut(i) {
                *slot = row.clone();
            }
            if (lineage as usize) < self.lineages.len() {
                used.resize(self.lineages.len(), false);
                used[lineage as usize] = true;
            }
            // remember the file as it was when it was opened, so a file that changed meanwhile is looked at again
            if let Some((okey, size, mtime_ns)) = opened_key {
                seen_now.insert(
                    okey,
                    Seen {
                        size,
                        mtime_ns,
                        gz: f.gz,
                        lineage,
                        row,
                    },
                );
            }
            self.evict_if_over(false);
            progress.tick(self);
            self.last_progress = Instant::now();
        }
        if cancelled || cancel.load(Ordering::Relaxed) {
            // keep what is known about files this pass did not reach
            for (k, v) in std::mem::take(&mut self.seen) {
                seen_now.entry(k).or_insert(v);
            }
            self.seen = seen_now;
            return false;
        }
        self.seen = seen_now;
        used.resize(self.lineages.len(), false);
        for (l, u) in self.lineages.iter_mut().zip(used) {
            if !u {
                // its file is gone: the events stay, their source lines can no longer be read
                l.file = None;
            }
        }
        true
    }

    fn row_of(&self, f: &Listed, size: u64, lineage: u32, scanned: u64) -> FileRow {
        let l = self.lineages.get(lineage as usize);
        let state = match l {
            Some(l) if l.failed => FState::Error,
            Some(l) if l.start_offset > 0 || l.stopped_by_budget || !l.at_eof => FState::Partial,
            None | Some(_) => FState::Indexed,
        };
        FileRow {
            name: f.name.clone(),
            bytes: size,
            scanned_bytes: scanned.min(size),
            compressed: f.gz,
            events: l.map_or(0, |l| l.events),
            skipped_lines: l.map_or(0, |l| l.skipped_lines),
            state,
            first_us: l.map_or(NO_TS, |l| l.first_us),
            last_us: l.map_or(NO_TS, |l| l.last_us),
        }
    }

    fn plain_row(f: &Listed, size: u64, state: FState) -> FileRow {
        FileRow {
            name: f.name.clone(),
            bytes: size,
            scanned_bytes: 0,
            compressed: f.gz,
            events: 0,
            skipped_lines: 0,
            state,
            first_us: NO_TS,
            last_us: NO_TS,
        }
    }

    /// Bytes of never-seen content this file may use, or `None` when it is left out.
    fn allowance(&mut self, f: &Listed) -> Option<u64> {
        if f.rot > 0 && self.evicted_spans > 0 {
            // the span limit already dropped newer traces than this rotation can hold
            self.cap_skipped += 1;
            self.truncated = true;
            return None;
        }
        let a = if f.rot == 0 {
            self.budget_left.max(LIVE_FLOOR)
        } else {
            self.budget_left
        };
        if a == 0 {
            self.budget_skipped += 1;
            self.truncated = true;
            return None;
        }
        Some(a)
    }

    #[allow(clippy::type_complexity)]
    fn read_file(
        &mut self,
        dir: &Path,
        f: &Listed,
        row_index: usize,
        cancel: &AtomicBool,
        progress: &mut dyn Progress,
    ) -> (FileRow, u32, Option<((u64, u64), u64, i128)>) {
        let Ok(opened) = open_in(dir, &f.name) else {
            self.file_errors += 1;
            return (Self::plain_row(f, f.size, FState::Error), NO_ROW, None);
        };
        let okey = Some(((opened.dev, opened.ino), opened.size, opened.mtime_ns));
        if opened.size == 0 {
            return (Self::plain_row(f, 0, FState::Indexed), NO_ROW, okey);
        }
        let (row, lineage) = if f.gz {
            self.read_gz(dir, opened, f, row_index, cancel, progress)
        } else {
            self.read_plain(opened, f, row_index, cancel, progress)
        };
        if let Some(l) = self.lineages.get_mut(lineage as usize) {
            l.file = Some(FileRef {
                name: f.name.clone(),
                gz: f.gz,
            });
        }
        (row, lineage, okey)
    }

    fn read_plain(
        &mut self,
        opened: Opened,
        f: &Listed,
        row_index: usize,
        cancel: &AtomicBool,
        progress: &mut dyn Progress,
    ) -> (FileRow, u32) {
        let Opened { mut file, size, .. } = opened;
        let head_len = usize::try_from(size).unwrap_or(HEAD).min(HEAD);
        let head = match read_at(&file, 0, head_len) {
            Ok(h) if !h.is_empty() => h,
            _ => {
                self.file_errors += 1;
                return (Self::plain_row(f, size, FState::Error), NO_ROW);
            }
        };
        let known = self.find_lineage(&head).filter(|&l| {
            self.lineages.get(l as usize).is_some_and(|lin| {
                if lin.anchor_len == 0 {
                    return lin.consumed <= size;
                }
                read_at(&file, lin.anchor_off, lin.anchor_len as usize).is_ok_and(|a| {
                    a.len() == lin.anchor_len as usize && hash128(&a) == lin.anchor_hash
                })
            })
        });
        let lineage = match known {
            Some(l) => l,
            None => {
                let Some(allow) = self.allowance(f) else {
                    return (Self::plain_row(f, size, FState::Skipped), NO_ROW);
                };
                let start = size.saturating_sub(allow);
                self.budget_left = self.budget_left.saturating_sub(size - start);
                if start > 0 {
                    self.truncated = true;
                    self.budget_cut += 1;
                }
                match self.new_lineage(&head, start) {
                    Some(l) => l,
                    None => return (Self::plain_row(f, size, FState::Error), NO_ROW),
                }
            }
        };
        let Some(lin) = self.lineages.get(lineage as usize) else {
            return (Self::plain_row(f, size, FState::Error), NO_ROW);
        };
        let (from, start_offset, fresh) = (lin.consumed, lin.start_offset, known.is_none());
        if from >= size && !fresh {
            // nothing new (a copy of content that was read in full)
            return (
                self.row_of(f, size, lineage, size - start_offset.min(size)),
                lineage,
            );
        }
        if file.seek(SeekFrom::Start(from)).is_err() {
            self.file_errors += 1;
            return (Self::plain_row(f, size, FState::Error), lineage);
        }
        let mut lr = LineReader::new(file, from, self.limits.max_line_bytes);
        let mut failed = false;
        if fresh && start_offset > 0 {
            // the read starts in the middle of a line: drop the rest of that line
            failed = lr.skip_partial_line().is_err();
        }
        let end = if failed {
            Err(())
        } else {
            self.consume(
                &mut lr,
                lineage,
                None,
                cancel,
                progress,
                row_index,
                &|_, off| off.saturating_sub(start_offset),
            )
            .map_err(|_| ())
        };
        if let Some(l) = self.lineages.get_mut(lineage as usize) {
            match end {
                Ok(End::Eof | End::TornTail) => l.at_eof = true,
                Ok(End::Budget | End::Cancelled) => l.at_eof = false,
                Err(()) => {
                    l.failed = true;
                    self.file_errors += 1;
                }
            }
            // the content was short when first seen: remember its full head now
            if (l.head_len as usize) < HEAD
                && l.consumed > u64::from(l.head_len)
                && let Ok(h) = read_at(lr.get_ref(), 0, HEAD)
            {
                l.head_len = u32::try_from(h.len()).unwrap_or(0);
                l.head_hash = hash128(&h);
            }
        }
        let scanned = self
            .lineages
            .get(lineage as usize)
            .map_or(0, |l| l.consumed.saturating_sub(l.start_offset));
        (self.row_of(f, size, lineage, scanned), lineage)
    }

    fn read_gz(
        &mut self,
        dir: &Path,
        opened: Opened,
        f: &Listed,
        row_index: usize,
        cancel: &AtomicBool,
        progress: &mut dyn Progress,
    ) -> (FileRow, u32) {
        let size = opened.size;
        let key = (opened.dev, opened.ino, opened.size, opened.mtime_ns);
        // the last four bytes of a gzip member: the length of what it holds, modulo 2^32
        let isize_tail = read_at(&opened.file, size.saturating_sub(4), 4)
            .ok()
            .filter(|b| b.len() == 4)
            .map(|b| u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
        let gz =
            GzReader::with_checkpoints(opened.file, self.limits.checkpoint_every, CHECKPOINT_CAP);
        let mut lr = LineReader::new(gz, 0, self.limits.max_line_bytes);
        let head = match lr.peek_head(HEAD) {
            Ok(h) if !h.is_empty() => h.to_vec(),
            _ => {
                self.file_errors += 1;
                return (Self::plain_row(f, size, FState::Error), NO_ROW);
            }
        };
        let scanned = |r: &GzReader<std::fs::File>, _off: u64| r.compressed_bytes();
        if let Some(l) = self.find_lineage(&head) {
            let Some(lin) = self.lineages.get(l as usize).cloned() else {
                return (Self::plain_row(f, size, FState::Error), NO_ROW);
            };
            if lin.stopped_by_budget {
                // the budget ended this content once: it is not read again (it stays `partial`)
                return (self.row_of(f, size, l, lr.get_ref().compressed_bytes()), l);
            }
            if lin.at_eof && isize_tail == Some(lin.consumed & 0xffff_ffff) {
                // the compressed copy of a rotation that was read to its end: nothing to index, but its content
                // reads need seek points into THIS file (once per file)
                if lin.gz_points.as_ref().is_none_or(|p| p.key != key) {
                    // never past the content that was read (the trailer that led here is the file's own claim)
                    Self::drain(&mut lr, lin.consumed, cancel);
                    if !cancel.load(Ordering::Relaxed) {
                        self.keep_points(l, key, &mut lr);
                    }
                }
                return (self.row_of(f, size, l, size), l);
            }
            // go to where the scan stopped, checking on the way that this really is the same content
            let mut anchor: Vec<u8> = Vec::new();
            let (a0, a1) = (lin.anchor_off, lin.anchor_off + u64::from(lin.anchor_len));
            let skipped = lr.skip_bytes(lin.consumed, |off, bytes| {
                let end = off + bytes.len() as u64;
                if end > a0 && off < a1 {
                    let from = usize::try_from(a0.saturating_sub(off)).unwrap_or(0);
                    let to = usize::try_from(a1.min(end) - off).unwrap_or(0);
                    if from < to && to <= bytes.len() {
                        anchor.extend_from_slice(&bytes[from..to]);
                    }
                }
            });
            let same = matches!(skipped, Ok(n) if n == lin.consumed)
                && (lin.anchor_len == 0
                    || (anchor.len() == lin.anchor_len as usize
                        && hash128(&anchor) == lin.anchor_hash));
            if same {
                // what is new in it counts against the scan budget like any other content
                let before = lin.consumed;
                let allow = self.budget_left;
                let end = self.consume(
                    &mut lr,
                    l,
                    Some(allow),
                    cancel,
                    progress,
                    row_index,
                    &scanned,
                );
                let used = self
                    .lineages
                    .get(l as usize)
                    .map_or(0, |x| x.consumed.saturating_sub(before));
                self.budget_left = self.budget_left.saturating_sub(used);
                self.finish_gz(l, end.as_ref().map_err(std::io::Error::kind));
                self.keep_points(l, key, &mut lr);
                return (self.row_of(f, size, l, lr.get_ref().compressed_bytes()), l);
            }
            // it only starts the same: read it as new content, from the beginning
            let Ok(again) = open_in(dir, &f.name) else {
                self.file_errors += 1;
                return (Self::plain_row(f, size, FState::Error), NO_ROW);
            };
            let gz = GzReader::with_checkpoints(
                again.file,
                self.limits.checkpoint_every,
                CHECKPOINT_CAP,
            );
            lr = LineReader::new(gz, 0, self.limits.max_line_bytes);
        }
        let Some(allow) = self.allowance(f) else {
            return (Self::plain_row(f, size, FState::Skipped), NO_ROW);
        };
        // a rotation of several gigabytes cannot be read from its end, so one the trailer says is larger than the
        // budget is left out instead of read from its oldest part. The trailer is the file's own claim, so it is a
        // hint only: whatever is read is counted as it is decompressed and stops at the budget (`consume`)
        if f.rot > 0
            && let Some(est) = isize_tail.filter(|e| *e >= size)
            && est > allow
        {
            self.budget_skipped += 1;
            self.truncated = true;
            return (Self::plain_row(f, size, FState::Skipped), NO_ROW);
        }
        let Some(l) = self.new_lineage(&head, 0) else {
            return (Self::plain_row(f, size, FState::Error), NO_ROW);
        };
        let end = self.consume(
            &mut lr,
            l,
            Some(allow),
            cancel,
            progress,
            row_index,
            &scanned,
        );
        let used = self.lineages.get(l as usize).map_or(0, |x| x.consumed);
        self.budget_left = self.budget_left.saturating_sub(used);
        self.finish_gz(l, end.as_ref().map_err(std::io::Error::kind));
        self.keep_points(l, key, &mut lr);
        (self.row_of(f, size, l, lr.get_ref().compressed_bytes()), l)
    }

    /// Decompress the rest of a stream without reading lines (only its seek points are wanted).
    fn drain(lr: &mut LineReader<GzReader<std::fs::File>>, upto: u64, cancel: &AtomicBool) {
        const STEP: u64 = 8 * 1024 * 1024;
        let mut left = upto;
        while left > 0 && !cancel.load(Ordering::Relaxed) {
            let step = left.min(STEP);
            match lr.skip_bytes(step, |_, _| {}) {
                Ok(n) if n == step => left -= n,
                _ => break,
            }
        }
    }

    /// Keep the seek points a read of the file with identity `key` recorded (every read of a `.gz` starts at its
    /// beginning). A shorter set for the same file (a read that stopped early) does not replace a longer one.
    fn keep_points(
        &mut self,
        lineage: u32,
        key: (u64, u64, u64, i128),
        lr: &mut LineReader<GzReader<std::fs::File>>,
    ) {
        let (points, _) = lr.get_mut().take_checkpoints();
        let Some(l) = self.lineages.get_mut(lineage as usize) else {
            return;
        };
        let reach = |p: &[GzCheckpoint]| p.last().map_or(0, |c| c.out);
        if let Some(old) = &l.gz_points
            && old.key == key
            && reach(&old.points) > reach(&points)
        {
            return;
        }
        l.gz_points = Some(Arc::new(GzPoints { key, points }));
    }

    fn finish_gz(&mut self, lineage: u32, end: Result<&End, std::io::ErrorKind>) {
        let Some(l) = self.lineages.get_mut(lineage as usize) else {
            return;
        };
        match end {
            Ok(End::Eof | End::TornTail) => l.at_eof = true,
            Ok(End::Budget) => {
                l.at_eof = false;
                l.stopped_by_budget = true;
                self.truncated = true;
                self.budget_cut += 1;
            }
            // a rotation that is still being compressed ends early: keep what was read, look again when it grows
            Ok(End::Cancelled) | Err(std::io::ErrorKind::UnexpectedEof) => l.at_eof = false,
            Err(_) => {
                l.failed = true;
                self.file_errors += 1;
            }
        }
    }

    /// Read lines until the stream ends: every event goes into the index with the place of its line.
    #[allow(clippy::too_many_arguments)]
    fn consume<R: Read>(
        &mut self,
        lr: &mut LineReader<R>,
        lineage: u32,
        byte_limit: Option<u64>,
        cancel: &AtomicBool,
        progress: &mut dyn Progress,
        row_index: usize,
        scanned: &dyn Fn(&R, u64) -> u64,
    ) -> std::io::Result<End> {
        let began = lr.offset();
        let mut events = 0u64;
        let mut skipped = 0u64;
        let (mut first_us, mut last_us) = (NO_TS, NO_TS);
        let mut consumed = began;
        let mut anchor = [0u8; ANCHOR];
        let (mut anchor_len, mut anchor_off) = (0usize, 0u64);
        let mut lines = 0u64;
        let mut result = Ok(End::Eof);
        loop {
            lines += 1;
            if cancel.load(Ordering::Relaxed) {
                result = Ok(End::Cancelled);
                break;
            }
            // the budget counts the bytes of content read (decompressed, for a .gz), checked at every line
            if byte_limit.is_some_and(|max| consumed - began >= max) {
                result = Ok(End::Budget);
                break;
            }
            if lines.is_multiple_of(512) {
                let marks_cap = self.limits.max_spans.saturating_mul(MARKS_PER_SPAN);
                let over = self.ing.store.spans.len()
                    > self.limits.max_spans + self.limits.max_spans / 4 + 1024
                    || self.ing.store.marks.len() > marks_cap + marks_cap / 4;
                if over || self.last_progress.elapsed() >= PROGRESS_EVERY {
                    self.note(lineage, events, skipped, first_us, last_us, consumed, None);
                    (events, skipped) = (0, 0);
                    if over {
                        self.evict_if_over(true);
                    }
                    if let Some(row) = self.files.get_mut(row_index) {
                        row.scanned_bytes = scanned(lr.get_ref(), consumed).min(row.bytes);
                        if let Some(l) = self.lineages.get(lineage as usize) {
                            row.events = l.events;
                            row.skipped_lines = l.skipped_lines;
                            row.first_us = l.first_us;
                            row.last_us = l.last_us;
                        }
                    }
                    progress.tick(self);
                    self.last_progress = Instant::now();
                }
            }
            // inside one long line too: stop at the budget (plus a little) or when the pass is cancelled
            let stop = |at: u64| {
                cancel.load(Ordering::Relaxed)
                    || byte_limit.is_some_and(|max| at - began > max.saturating_add(BUDGET_SLACK))
            };
            let line = match lr.next_line_until(&stop) {
                Ok(l) => l,
                Err(e) if is_stopped(&e) => {
                    result = Ok(if cancel.load(Ordering::Relaxed) {
                        End::Cancelled
                    } else {
                        End::Budget
                    });
                    break;
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            match line {
                Line::Eof => break,
                Line::Oversize {
                    start,
                    consumed: n,
                    terminated,
                } => {
                    if !terminated {
                        result = Ok(End::TornTail);
                        break;
                    }
                    self.ing.counters.oversize_lines += 1;
                    skipped += 1;
                    consumed = start + n;
                }
                Line::Data {
                    bytes,
                    start,
                    consumed: n,
                    terminated,
                } => match parse_line(bytes) {
                    Parsed::Blank => consumed = start + n,
                    Parsed::Malformed => {
                        if !terminated {
                            result = Ok(End::TornTail);
                            break;
                        }
                        self.ing.counters.malformed_lines += 1;
                        skipped += 1;
                        consumed = start + n;
                    }
                    Parsed::Event(ev) => {
                        let loc = Loc {
                            offset: start,
                            len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
                            lineage,
                        };
                        if let Some(ts) = ev.timestamp.0 {
                            if first_us == NO_TS || ts < first_us {
                                first_us = ts;
                            }
                            if last_us == NO_TS || ts > last_us {
                                last_us = ts;
                            }
                        }
                        self.ing.event(&ev, loc);
                        events += 1;
                        consumed = start + n;
                        anchor_len = bytes.len().min(ANCHOR);
                        anchor[..anchor_len].copy_from_slice(&bytes[bytes.len() - anchor_len..]);
                        anchor_off = start + (bytes.len() - anchor_len) as u64;
                    }
                },
            }
            lr.release_spill();
        }
        let a = (anchor_len > 0).then(|| (anchor_off, &anchor[..anchor_len]));
        self.note(lineage, events, skipped, first_us, last_us, consumed, a);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn note(
        &mut self,
        lineage: u32,
        events: u64,
        skipped: u64,
        first_us: i64,
        last_us: i64,
        consumed: u64,
        anchor: Option<(u64, &[u8])>,
    ) {
        let Some(l) = self.lineages.get_mut(lineage as usize) else {
            return;
        };
        l.events += events;
        l.skipped_lines += skipped;
        if first_us != NO_TS && (l.first_us == NO_TS || first_us < l.first_us) {
            l.first_us = first_us;
        }
        if last_us != NO_TS && (l.last_us == NO_TS || last_us > l.last_us) {
            l.last_us = last_us;
        }
        l.consumed = l.consumed.max(consumed);
        if let Some((off, bytes)) = anchor {
            l.anchor_off = off;
            l.anchor_len = u32::try_from(bytes.len()).unwrap_or(0);
            l.anchor_hash = hash128(bytes);
        }
    }

    /// Keep the index within `max_spans`: when it is over, the oldest traces go until a tenth of the limit is free
    /// (so the next few passes do not have to do this again). `mid_file`: called from inside a long read, where a
    /// quarter over the limit is tolerated before anything is dropped.
    pub fn evict_if_over(&mut self, mid_file: bool) {
        // marks: at most MARKS_PER_SPAN per span of the limit, the oldest dropped first
        let mark_cap = self.limits.max_spans.saturating_mul(MARKS_PER_SPAN);
        let mark_limit = if mid_file {
            mark_cap + mark_cap / 4
        } else {
            mark_cap
        };
        if self.ing.store.marks.len() > mark_limit {
            let dropped = super::derive::evict_marks(&mut self.ing, mark_cap - mark_cap / 10);
            self.evicted_marks += dropped as u64;
            if dropped > 0 {
                self.truncated = true;
            }
        }
        let live = self.ing.store.spans.iter().filter(|s| s.live()).count();
        let limit = if mid_file {
            self.limits.max_spans + self.limits.max_spans / 4
        } else {
            self.limits.max_spans
        };
        if live <= limit {
            return;
        }
        let target = self.limits.max_spans - self.limits.max_spans / 10;
        let dropped = super::derive::evict(&mut self.ing, target);
        self.evicted_spans += dropped as u64;
        if dropped > 0 {
            self.truncated = true;
        }
    }
}
