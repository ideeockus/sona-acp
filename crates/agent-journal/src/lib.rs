//! Agent Journal in daily JSONL files (D17, D74).
//!
//! ```text
//! journal/
//!   2026-11-03.jsonl.gz   closed days, compressed, kept for the retention period
//!   2026-11-11.jsonl      today, one entry per line
//! ```

mod files;

use async_trait::async_trait;
use control_plane_common::journal::{
    Event, JournalEntry, JournalFilter, JournalReader, JournalWriter,
};
use control_plane_common::{Caller, Error, Result, Timestamp};
use jiff::civil::Date;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Where the journal takes the time from. `Timestamp::now` outside tests.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// The single writer of the journal (D17).
pub struct FileJournal {
    state: Arc<Mutex<State>>,
}

struct State {
    dir: PathBuf,
    retention_days: u32,
    clock: Clock,
    day: Date,
    file: File,
}

impl FileJournal {
    /// Opens today's file in `dir`, compressing closed days and dropping expired ones.
    pub async fn open(dir: PathBuf, retention_days: u32, clock: Clock) -> Result<Self> {
        let state = blocking(move || {
            std::fs::create_dir_all(&dir).map_err(io)?;
            let day = files::day_of(clock());
            files::housekeep(&dir, day, retention_days)?;
            let file = files::open_day(&dir, day)?;
            Ok(State {
                dir,
                retention_days,
                clock,
                day,
                file,
            })
        })
        .await?;
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }
}

impl State {
    fn append(&mut self, event: Event) -> Result<()> {
        let timestamp = (self.clock)();
        let day = files::day_of(timestamp);
        if day != self.day {
            self.file = files::open_day(&self.dir, day)?;
            self.day = day;
            files::housekeep(&self.dir, day, self.retention_days)?;
        }
        let mut line = serde_json::to_string(&JournalEntry { timestamp, event })
            .map_err(|e| Error::Internal(e.to_string()))?;
        line.push('\n');
        // One write per line, so a crash can cut only the last line.
        self.file.write_all(line.as_bytes()).map_err(io)
    }
}

#[async_trait]
impl JournalWriter for FileJournal {
    async fn record(&self, event: Event) -> Result<()> {
        let state = self.state.clone();
        blocking(move || lock(&state)?.append(event)).await
    }
}

#[async_trait]
impl JournalReader for FileJournal {
    async fn read(&self, caller: &Caller, filter: JournalFilter) -> Result<Vec<JournalEntry>> {
        if !matches!(caller, Caller::Operator { .. }) {
            return Err(Error::Forbidden(format!(
                "{} is not the operator",
                caller.alias()
            )));
        }
        let state = self.state.clone();
        blocking(move || {
            // Holding the lock keeps housekeeping from moving files mid-read.
            let state = lock(&state)?;
            let mut entries = Vec::new();
            for path in files::all_days(&state.dir)? {
                for line in files::read_lines(&path)? {
                    // A line cut by a crash does not parse and is skipped.
                    if let Ok(entry) = serde_json::from_str::<JournalEntry>(&line)
                        && filter.matches(&entry.event)
                    {
                        entries.push(entry);
                    }
                }
            }
            Ok(entries)
        })
        .await
    }
}

/// Runs file work off the async threads.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Internal(e.to_string()))?
}

fn lock(state: &Mutex<State>) -> Result<std::sync::MutexGuard<'_, State>> {
    state
        .lock()
        .map_err(|_| Error::Internal("journal lock poisoned".to_owned()))
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}
