//! Day files: naming, compression, retention and reading.

use crate::io;
use control_plane_common::{Result, Timestamp};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use jiff::ToSpan;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

const OPEN: &str = ".jsonl";
const CLOSED: &str = ".jsonl.gz";

/// The UTC day of an instant.
pub fn day_of(timestamp: Timestamp) -> Date {
    timestamp.as_jiff().to_zoned(TimeZone::UTC).date()
}

pub fn open_day(dir: &Path, day: Date) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{day}{OPEN}")))
        .map_err(io)
}

/// Compresses every closed day and deletes compressed days older than the retention.
pub fn housekeep(dir: &Path, today: Date, retention_days: u32) -> Result<()> {
    let oldest_kept = today
        .checked_sub(i64::from(retention_days).days())
        .map_err(|e| control_plane_common::Error::Internal(e.to_string()))?;

    for (day, path) in days(dir)? {
        let name = file_name(&path);
        if name.ends_with(CLOSED) {
            if day < oldest_kept {
                fs::remove_file(&path).map_err(io)?;
            }
        } else if day < today {
            compress(&path)?;
        }
    }
    Ok(())
}

/// All day files, oldest first.
pub fn all_days(dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(days(dir)?.into_iter().map(|(_, path)| path).collect())
}

pub fn read_lines(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).map_err(io)?;
    let reader: Box<dyn Read> = if file_name(path).ends_with(CLOSED) {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    BufReader::new(reader)
        .lines()
        .collect::<std::io::Result<_>>()
        .map_err(io)
}

/// Day files in `dir` with their dates, oldest first. Other files are ignored.
fn days(dir: &Path) -> Result<Vec<(Date, PathBuf)>> {
    let mut days = Vec::new();
    for entry in fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        let name = file_name(&path);
        let stem = name
            .strip_suffix(CLOSED)
            .or_else(|| name.strip_suffix(OPEN));
        if let Some(day) = stem.and_then(|stem| stem.parse::<Date>().ok()) {
            days.push((day, path));
        }
    }
    days.sort();
    Ok(days)
}

/// Replaces `<day>.jsonl` with `<day>.jsonl.gz`. Safe to repeat after a crash.
fn compress(path: &Path) -> Result<()> {
    let closed = path.with_extension("jsonl.gz");
    if !closed.exists() {
        let partial = path.with_extension("jsonl.gz.partial");
        let mut encoder =
            GzEncoder::new(File::create(&partial).map_err(io)?, Compression::default());
        std::io::copy(&mut File::open(path).map_err(io)?, &mut encoder).map_err(io)?;
        encoder.finish().map_err(io)?.sync_all().map_err(io)?;
        fs::rename(&partial, &closed).map_err(io)?;
    }
    fs::remove_file(path).map_err(io)
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
}
