//! Data Designer run artifacts on disk (workstream C): `records.jsonl` (the ONLY dataset file the console reads; never
//! parquet), the user's dataset copy (csv / jsonl / json framed from the exporter's lines, parquet batches copied with
//! `shutil.copy2` semantics), the run-files listing (`os.walk`), and the preview / batch file discovery. Every byte the
//! Python adapter writes itself is written the same way here (binary line copies, `b"[\n"` / `b",\n"` / `b"\n]\n"`
//! framing, `bytes.strip()` on json lines, csv header skipping).

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::services::pyfmt::char_prefix;
use crate::services::support::{self as sp, full};

pub const MAX_PREVIEW_RECORDS: usize = 2000;
pub const MAX_RECORDS_ROWS: usize = 5000;
pub const CELL_CAP: usize = 2048;
pub const MAX_FILES: usize = 500;

static PREVIEW_DIR_RE: LazyLock<Regex> = LazyLock::new(|| full(r"preview_results_\d{8}_\d{6}"));

/// The longest line [`for_each_line`] hands over, and the most bytes it reads from one file (audit finding 9: a
/// newline-free or huge file must not become one unbounded allocation; Python would try and could be OOM-killed).
pub const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_READ_BYTES: u64 = 1024 * 1024 * 1024;

/// Iterate the lines of a binary file like Python's `for line in fh` (each line keeps its `\n`). `f` returns false
/// to stop early. The file is opened with O_NOFOLLOW (a symlink planted in a run directory is an error, not a
/// redirection); a line longer than [`MAX_LINE_BYTES`] ends the iteration, and so does [`MAX_READ_BYTES`] in all
/// (PORTING D-27).
pub fn for_each_line(path: &Path, mut f: impl FnMut(&[u8]) -> bool) -> std::io::Result<()> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = File::options()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    let mut r = BufReader::new(file.take(MAX_READ_BYTES));
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if (&mut r)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut buf)?
            == 0
        {
            return Ok(());
        }
        if buf.len() as u64 > MAX_LINE_BYTES || !f(&buf) {
            return Ok(());
        }
    }
}

/// `_read_records(path)`: (rows, columns in first-seen order). Malformed lines are skipped, never raised.
#[must_use]
pub fn read_records(path: &Path) -> (Vec<Map<String, Value>>, Vec<String>) {
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let res = for_each_line(path, |line| {
        if rows.len() >= MAX_RECORDS_ROWS {
            return false;
        }
        if let Some(Value::Object(d)) = sp::json_loads_bytes(line) {
            for k in d.keys() {
                if columns.len() < 64 && !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
            rows.push(d);
        }
        true
    });
    match res {
        Ok(()) => (rows, columns),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

/// `_cap_value(v)`: values pass through; no string exceeds CELL_CAP (nothing added, no marker fields).
#[must_use]
pub fn cap_value(v: &Value) -> Value {
    cap_depth(v, 0)
}

fn cap_depth(v: &Value, depth: usize) -> Value {
    match v {
        Value::String(s) => Value::String(char_prefix(s, CELL_CAP).to_owned()),
        Value::Object(m) if depth < sp::MAX_JSON_DEPTH => {
            let mut out = Map::new();
            for (k, x) in m.iter().take(50) {
                out.insert(char_prefix(k, 120).to_owned(), cap_depth(x, depth + 1));
            }
            Value::Object(out)
        }
        Value::Array(a) if depth < sp::MAX_JSON_DEPTH => {
            Value::Array(a.iter().take(50).map(|x| cap_depth(x, depth + 1)).collect())
        }
        other => other.clone(),
    }
}

/// `_append_capped(src, dst, kept)`: copy lines from src onto records.jsonl until MAX_PREVIEW_RECORDS lines are kept.
pub fn append_capped(src: &Path, dst: &Path, kept: usize) -> std::io::Result<usize> {
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dst)?;
    let mut kept = kept;
    let mut err = None;
    for_each_line(src, |line| {
        if kept >= MAX_PREVIEW_RECORDS {
            return false;
        }
        if let Err(e) = out.write_all(line) {
            err = Some(e);
            return false;
        }
        if line.ends_with(b"\n") {
            kept += 1;
        }
        true
    })?;
    match err {
        Some(e) => Err(e),
        None => Ok(kept),
    }
}

/// `bytes.strip()`: ASCII whitespace `b" \t\n\r\x0b\x0c"` (Rust's `trim_ascii` leaves `\x0b`).
#[must_use]
pub fn py_bytes_strip(b: &[u8]) -> &[u8] {
    let ws = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !ws(c)).map_or(start, |i| i + 1);
    &b[start..end.max(start)]
}

/// `_append_user_copy(src, target, fmt, first_batch, wrote_any)`: csv skips repeated headers, jsonl appends lines,
/// json streams object lines that the caller frames into one array. Returns the new `wrote_any`.
pub fn append_user_copy(
    src: &Path,
    target: &Path,
    fmt: &str,
    first_batch: bool,
    wrote_any: bool,
) -> std::io::Result<bool> {
    let input = File::open(src)?;
    let mut out = if first_batch {
        File::create(target)?
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(target)?
    };
    if fmt == "json" && first_batch {
        out.write_all(b"[\n")?;
    }
    let skip_header = fmt == "csv" && !first_batch;
    let mut wrote_any = wrote_any;
    let mut r = BufReader::new(input);
    let mut buf = Vec::new();
    let mut i = 0usize;
    loop {
        buf.clear();
        if r.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let line = &buf[..];
        let first = i == 0;
        i += 1;
        if skip_header && first {
            continue;
        }
        if fmt == "json" {
            let stripped = py_bytes_strip(line);
            if stripped.is_empty() {
                continue;
            }
            if wrote_any {
                out.write_all(b",\n")?;
            }
            out.write_all(stripped)?;
            wrote_any = true;
        } else {
            out.write_all(line)?;
        }
    }
    Ok(wrote_any)
}

/// `shutil.copy2(src, dst, follow_symlinks=False)` for a regular file: the bytes, the permission bits and the
/// access / modification times (extended attributes are not copied: PORTING.md workstream C deviations).
pub fn copy2(src: &Path, dst: &Path) -> std::io::Result<()> {
    let md = std::fs::symlink_metadata(src)?;
    if md.file_type().is_symlink() {
        // follow_symlinks=False: the LINK is copied, never what it points to (audit finding 9)
        return std::os::unix::fs::symlink(std::fs::read_link(src)?, dst);
    }
    std::fs::copy(src, dst)?;
    std::fs::set_permissions(
        dst,
        std::fs::Permissions::from_mode(md.permissions().mode() & 0o7777),
    )?;
    let times = std::fs::FileTimes::new()
        .set_accessed(md.accessed()?)
        .set_modified(md.modified()?);
    File::options().write(true).open(dst)?.set_times(times)
}

/// `_newest_preview_dir(run_dir)`: the newest preview_results_<stamp>/ the CLI wrote (regular dirs only).
#[must_use]
pub fn newest_preview_dir(run_dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(run_dir) {
        for e in rd.flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let p = e.path();
            if PREVIEW_DIR_RE.is_match(&name) && !p.is_symlink() && p.is_dir() {
                found.push((name, p));
            }
        }
    }
    let mut best: Option<(String, PathBuf)> = None;
    for (n, p) in found {
        if best.as_ref().is_none_or(|(b, _)| n > *b) {
            best = Some((n, p));
        }
    }
    best.map(|(_, p)| p)
}

/// `_batch_files(data_dir, md)`: metadata.json -> file_paths["parquet-files"], each a contained regular file.
#[must_use]
pub fn batch_files(data_dir: &Path, md: &Map<String, Value>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let base = sp::realpath(data_dir);
    let rels = sp::list(sp::dict(md.get("file_paths")).get("parquet-files"));
    for rel in rels.iter().take(64) {
        let Value::String(rel) = rel else { continue };
        if rel.is_empty() || rel.starts_with('/') {
            continue;
        }
        let p = sp::realpath(&data_dir.join(rel));
        if p.starts_with(&base) && !p.is_symlink() && p.is_file() {
            out.push(p);
        }
    }
    out
}

/// `run_files`: rel paths + sizes of every regular file in the run dir (`os.walk`, top-down, no symlinks, at most
/// 64 sub-directories and 128 files per directory, MAX_FILES in all; records.jsonl always counted).
#[must_use]
pub fn walk_files(run_dir: &Path) -> Vec<(String, u64)> {
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![run_dir.to_path_buf()];
    'walk: while let Some(top) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&top) else {
            continue;
        };
        let mut dirs: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for e in rd {
            // os.walk skips a directory whose listing fails part way (onerror=None)
            let Ok(e) = e else { continue 'walk };
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                dirs.push(name);
            } else {
                names.push(name);
            }
        }
        let dirs: Vec<String> = dirs
            .into_iter()
            .filter(|d| !top.join(d).is_symlink())
            .take(64)
            .collect();
        names.sort();
        for name in names.into_iter().take(128) {
            let p = top.join(&name);
            if p.is_symlink() || !p.is_file() {
                continue;
            }
            let rel = p
                .strip_prefix(run_dir)
                .map(|r| r.to_string_lossy().into_owned())
                .unwrap_or_default();
            if rel == "records.jsonl" || files.len() < MAX_FILES {
                match std::fs::metadata(&p) {
                    Ok(md) => files.push((rel, md.len())),
                    Err(_) => break 'walk,
                }
            }
        }
        for d in dirs.into_iter().rev() {
            stack.push(top.join(d));
        }
    }
    files.truncate(MAX_FILES);
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_strip_like_python() {
        assert_eq!(py_bytes_strip(b"\x0b {\"a\": 1}\r\n"), b"{\"a\": 1}");
        assert_eq!(py_bytes_strip(b" \n"), b"");
    }
}
