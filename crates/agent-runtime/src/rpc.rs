//! Pi's RPC protocol: JSONL commands on stdin, JSONL records on stdout (D7).
//!
//! Only what Agent Runtime uses is modelled. Unknown records parse as [`Event::Other`],
//! so a newer Pi with extra events does not break us.

use control_plane_common::runtime::{ModelInfo, Price, Usage};
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
    /// The models Pi has a key for (D104).
    GetAvailableModels,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
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
        /// What the command returns, if anything.
        #[serde(default)]
        data: Option<serde_json::Value>,
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
    /// Pi started working on the prompt. It may start again within one Run: after
    /// a provider retry or a steer.
    AgentStart,
    /// A message is complete; for the model's answers it tells how they ended.
    MessageEnd { message: EndedMessage },
    /// Pi will not continue on its own: the Run's work is done.
    AgentSettled,
    #[serde(other)]
    Other,
}

/// The part of a finished message that Runtime looks at.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndedMessage {
    pub role: String,
    /// What the model's answer cost; absent on other messages.
    #[serde(default)]
    pub usage: Option<PiUsage>,
    /// `error` once Pi gave up on the model provider.
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
}

/// Pi's usage of one model answer, as much of it as Runtime keeps (D97).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PiUsage {
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    pub cost: PiCost,
}

/// In US dollars.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PiCost {
    pub total: f64,
}

impl PiUsage {
    pub fn to_usage(&self) -> Usage {
        Usage {
            tokens_in: self.input + self.cache_read + self.cache_write,
            tokens_out: self.output,
            // A negative or broken cost counts as none rather than as a huge one.
            cost_micros: (self.cost.total * 1_000_000.0).round().max(0.0) as u64,
        }
    }
}

impl EndedMessage {
    /// The model's failure, if this is a model answer that ended in one.
    pub fn model_error(&self) -> Option<String> {
        (self.role == "assistant" && self.stop_reason.as_deref() == Some("error")).then(|| {
            self.error_message
                .clone()
                .unwrap_or_else(|| "no details".to_owned())
        })
    }
}

/// The answer to `get_available_models`: `{"models": [Model]}`.
pub fn available_models(data: serde_json::Value) -> Result<Vec<ModelInfo>> {
    #[derive(Deserialize)]
    struct Available {
        models: Vec<PiModel>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PiModel {
        provider: String,
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        context_window: u64,
        #[serde(default)]
        cost: Option<PiPrice>,
    }
    #[derive(Deserialize)]
    struct PiPrice {
        #[serde(default)]
        input: f64,
        #[serde(default)]
        output: f64,
    }
    let available: Available = serde_json::from_value(data)
        .map_err(|e| Error::Internal(format!("invalid model list from Pi: {e}")))?;
    Ok(available
        .models
        .into_iter()
        .map(|m| ModelInfo {
            name: if m.name.is_empty() {
                m.id.clone()
            } else {
                m.name
            },
            provider: m.provider,
            id: m.id,
            context_window: m.context_window,
            price: m.cost.map_or_else(Price::default, |c| Price {
                input: c.input,
                output: c.output,
            }),
        })
        .collect())
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
