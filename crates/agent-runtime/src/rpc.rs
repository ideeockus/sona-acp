//! Pi's RPC protocol: JSONL commands on stdin, JSONL records on stdout (D7).
//!
//! Only what Agent Runtime uses is modelled. Unknown records parse as [`Event::Other`],
//! so a newer Pi with extra events does not break us.

use control_plane_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Starts work on a message.
    Prompt { message: String },
    /// Delivers a message after the current turn's tool calls, mid-run (D12).
    Steer { message: String },
    /// Stops the current run (D27).
    Abort,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The answer to a command. Success means accepted, not finished.
    Response {
        command: String,
        success: bool,
        #[serde(default)]
        error: Option<String>,
    },
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        is_error: bool,
    },
    /// Pi will not continue on its own: the Run's work is done.
    AgentSettled,
    #[serde(other)]
    Other,
}

/// Writes commands to Pi's stdin.
pub struct CommandWriter<W> {
    stdin: W,
}

impl<W: AsyncWrite + Unpin> CommandWriter<W> {
    pub fn new(stdin: W) -> Self {
        Self { stdin }
    }

    pub async fn send(&mut self, command: &Command) -> Result<()> {
        let mut line = serde_json::to_vec(command).map_err(|e| Error::Internal(e.to_string()))?;
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(io)?;
        self.stdin.flush().await.map_err(io)
    }

    /// Closing stdin asks Pi to shut down in order.
    pub async fn close(mut self) -> Result<()> {
        self.stdin.shutdown().await.map_err(io)
    }
}

/// Reads records from Pi's stdout.
pub struct EventReader<R> {
    stdout: BufReader<R>,
    line: Vec<u8>,
}

impl<R: AsyncRead + Unpin> EventReader<R> {
    pub fn new(stdout: R) -> Self {
        Self {
            stdout: BufReader::new(stdout),
            line: Vec::new(),
        }
    }

    /// The next record, or `None` once Pi closes stdout.
    ///
    /// Cancel-safe, so it can wait in `select!`: a partly read line stays buffered
    /// and the next call continues it.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            // Split on LF only: U+2028 and U+2029 are valid inside JSON strings.
            let read = self
                .stdout
                .read_until(b'\n', &mut self.line)
                .await
                .map_err(io)?;
            if read == 0 && self.line.is_empty() {
                return Ok(None);
            }
            let line = std::mem::take(&mut self.line);
            let record = line.trim_ascii_end();
            if record.is_empty() {
                continue;
            }
            // Stdout is reserved for protocol records, so anything else is a broken Pi.
            return serde_json::from_slice(record)
                .map(Some)
                .map_err(|e| Error::Internal(format!("invalid Pi record: {e}")));
        }
    }
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}

#[cfg(test)]
mod tests;
