//! Rotates and reads service logs.
//!
//! Rotation follows pm2-logrotate: the log is copied to a timestamped file
//! (`<name>__YYYY-MM-DD_HH-mm-ss.log`) and then truncated in place, and only
//! the newest `keep` copies are retained. The log is never renamed: launchd
//! opens it once when it starts the job and hands the descriptor to the
//! process. Lines written between the copy and the truncate are lost.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Local};

/// pm2-logrotate's default dateFormat, YYYY-MM-DD_HH-mm-ss.
const STAMP: &str = "%Y-%m-%d_%H-%M-%S";
const STAMP_LEN: usize = "2006-01-02_15-04-05".len();

fn file_size(path: &Path) -> io::Result<Option<u64>> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Parses the part after `<name>__` of a rotated file name into
/// (timestamp, collision suffix).
fn parse_stamp(rest: &str) -> Option<(&str, u32)> {
    let rest = rest.strip_suffix(".log")?;
    let (stamp, suffix) = rest.split_at_checked(STAMP_LEN)?;
    let shape_ok = stamp.bytes().enumerate().all(|(i, c)| match i {
        4 | 7 | 13 | 16 => c == b'-',
        10 => c == b'_',
        _ => c.is_ascii_digit(),
    });
    let n = match suffix {
        "" => 0,
        s => s.strip_prefix('_')?.parse().ok()?,
    };
    shape_ok.then_some((stamp, n))
}

/// Rotated copies of the log at `path`, newest first.
pub fn rotated(path: &Path) -> io::Result<Vec<PathBuf>> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let prefix = format!(
        "{}__",
        path.file_stem().unwrap_or_default().to_string_lossy()
    );
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        r => r?,
    };
    let mut found = vec![];
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        // The exact stamp shape keeps service "t3" from claiming the log of
        // a service named "t3__x".
        if let Some((stamp, n)) = name.strip_prefix(&prefix).and_then(parse_stamp) {
            found.push(((stamp.to_string(), n), dir.join(&name)));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

/// Copies a non-empty log to `<name>__<now>.log`, truncates it, and deletes
/// all but the newest `keep` copies. A missing or empty log is left alone.
/// Reports whether it rotated.
pub fn rotate(path: &Path, keep: u64, now: DateTime<Local>) -> io::Result<bool> {
    if file_size(path)?.unwrap_or(0) == 0 {
        return Ok(false);
    }
    if keep > 0 {
        let base = path.with_extension("");
        let prefix = format!("{}__{}", base.display(), now.format(STAMP));
        let dst = (0..)
            .map(|i| {
                PathBuf::from(if i == 0 {
                    format!("{prefix}.log")
                } else {
                    format!("{prefix}_{i}.log")
                })
            })
            .find(|p| !p.exists())
            .expect("unbounded");
        let tmp = dst.with_extension("log.tmp");
        fs::copy(path, &tmp)?;
        fs::rename(&tmp, &dst)?;
    }
    File::options().write(true).open(path)?.set_len(0)?;
    for old in rotated(path)?.into_iter().skip(keep as usize) {
        match fs::remove_file(old) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(true)
}

/// Rotates when the log has reached `max_size`.
pub fn rotate_if_needed(
    path: &Path,
    max_size: u64,
    keep: u64,
    now: DateTime<Local>,
) -> io::Result<bool> {
    match file_size(path)? {
        Some(size) if size >= max_size => rotate(path, keep, now),
        _ => Ok(false),
    }
}

/// The last `n` lines and the offset read up to. A missing file is empty.
pub fn tail(path: &Path, n: usize) -> io::Result<(Vec<String>, u64)> {
    let mut f = match File::open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((vec![], 0)),
        r => r?,
    };
    let size = f.seek(SeekFrom::End(0))?;
    if n == 0 || size == 0 {
        return Ok((vec![], size));
    }
    const CHUNK: u64 = 64 << 10;
    let mut buf: Vec<u8> = vec![];
    let mut pos = size;
    while pos > 0 && buf.iter().filter(|&&c| c == b'\n').count() <= n {
        let step = CHUNK.min(pos);
        pos -= step;
        let mut chunk = vec![0; step as usize];
        f.seek(SeekFrom::Start(pos))?;
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .strip_suffix('\n')
        .unwrap_or(&text)
        .split('\n')
        .map(String::from)
        .collect();
    if pos > 0 {
        lines.remove(0); // may be partial
    }
    let skip = lines.len().saturating_sub(n);
    Ok((lines.split_off(skip), size))
}

/// Emits complete lines appended after `offset`, forever. A file that
/// shrinks (rotated by truncation) is read again from the start.
pub fn follow(path: &Path, mut offset: u64, mut emit: impl FnMut(&str)) -> io::Result<()> {
    let mut partial = Vec::new();
    loop {
        if let Some(size) = file_size(path)? {
            if size < offset {
                offset = 0;
                partial.clear();
            }
            if size > offset {
                let mut f = File::open(path)?;
                f.seek(SeekFrom::Start(offset))?;
                let mut r = BufReader::new(f);
                loop {
                    let mut chunk = Vec::new();
                    let n = r.read_until(b'\n', &mut chunk)?;
                    offset += n as u64;
                    partial.extend_from_slice(&chunk);
                    if n == 0 || chunk.last() != Some(&b'\n') {
                        break;
                    }
                    partial.pop();
                    emit(&String::from_utf8_lossy(&partial));
                    partial.clear();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::sync::{Arc, Mutex};

    fn t0(hour: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 21, hour, 0, 0).unwrap()
    }

    fn names(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    // Requirement: How rotation works
    #[test]
    fn naming_retention_and_truncate_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t3.log");
        let other = dir.path().join("t3__x.log");
        fs::write(&other, "other service").unwrap();
        for (i, content) in ["one", "two", "three", "four"].iter().enumerate() {
            fs::write(&p, content).unwrap();
            let before = fs::metadata(&p).unwrap().ino();
            assert!(rotate(&p, 3, t0(14 + i as u32)).unwrap());
            let after = fs::metadata(&p).unwrap();
            assert_eq!(
                (after.ino(), after.len()),
                (before, 0),
                "truncated in place"
            );
        }
        let files = rotated(&p).unwrap();
        assert_eq!(
            names(&files),
            [
                "t3__2026-10-21_17-00-00.log",
                "t3__2026-10-21_16-00-00.log",
                "t3__2026-10-21_15-00-00.log"
            ]
        );
        assert_eq!(fs::read_to_string(&files[0]).unwrap(), "four");
        assert_eq!(fs::read_to_string(&other).unwrap(), "other service");
    }

    // Requirement: How rotation works
    #[test]
    fn same_second_gets_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        for c in ["a", "b", "c"] {
            fs::write(&p, c).unwrap();
            rotate(&p, 5, t0(14)).unwrap();
        }
        let files = rotated(&p).unwrap();
        assert_eq!(
            names(&files),
            [
                "x__2026-10-21_14-00-00_2.log",
                "x__2026-10-21_14-00-00_1.log",
                "x__2026-10-21_14-00-00.log"
            ]
        );
        assert_eq!(fs::read_to_string(&files[0]).unwrap(), "c");
    }

    // Requirement: Empty logs are not rotated
    #[test]
    fn empty_log_and_keep_zero() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        fs::write(&p, "").unwrap();
        assert!(!rotate(&p, 3, t0(14)).unwrap());
        fs::write(&p, "data").unwrap();
        assert!(rotate(&p, 0, t0(14)).unwrap());
        assert_eq!(fs::read_to_string(&p).unwrap(), "");
        assert!(rotated(&p).unwrap().is_empty());
    }

    // Requirement: Automatic rotation
    #[test]
    fn rotates_at_max_size() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        assert!(!rotate_if_needed(&p, 10, 3, t0(14)).unwrap());
        fs::write(&p, "small").unwrap();
        assert!(!rotate_if_needed(&p, 10, 3, t0(14)).unwrap());
        fs::write(&p, "exactly10!").unwrap();
        assert!(rotate_if_needed(&p, 10, 3, t0(14)).unwrap());
    }

    // Requirement: View logs
    #[test]
    fn tail_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        let mut text = String::new();
        for i in 0..20000 {
            text += &format!("line {}\n", "x".repeat(i % 7));
        }
        text += "last\n";
        fs::write(&p, &text).unwrap();
        let (lines, off) = tail(&p, 3).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "last");
        assert_eq!(off, text.len() as u64);
        assert!(tail(&dir.path().join("none"), 5).unwrap().0.is_empty());
    }

    // Requirement: Follow logs
    #[test]
    fn follow_survives_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        fs::write(&p, "old\n").unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let (p2, got2) = (p.clone(), got.clone());
        std::thread::spawn(move || follow(&p2, 4, |l| got2.lock().unwrap().push(l.to_string())));
        let mut f = File::options().append(true).open(&p).unwrap();
        let pause = || std::thread::sleep(Duration::from_millis(400));
        f.write_all(b"a\npart").unwrap();
        pause();
        f.write_all(b"ial\n").unwrap();
        pause();
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_len(0)
            .unwrap();
        pause();
        f.write_all(b"after\n").unwrap();
        pause();
        assert_eq!(*got.lock().unwrap(), ["a", "partial", "after"]);
    }
}
