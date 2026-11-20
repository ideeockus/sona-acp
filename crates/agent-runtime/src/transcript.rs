//! A Run's transcript, read from Pi's session file (D102).
//!
//! Pi keeps one JSONL file per session and appends every Run of it, so a Run is the
//! entries written between its start and its end. Lines we do not know are skipped:
//! a newer Pi may add entry types, and a transcript is for reading, not for replay.

use control_plane_common::runtime::{Step, TranscriptEntry};
use control_plane_common::{Error, Result, SessionKey, Timestamp};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

/// Longest text of one step; the rest is cut, so a read of a large file stays light.
const MAX_TEXT: usize = 16 * 1024;

/// The steps of the session written within `from..=to`, oldest first.
pub(crate) async fn read(
    sessions: &Path,
    session: SessionKey,
    from: Timestamp,
    to: Timestamp,
) -> Result<Vec<TranscriptEntry>> {
    let suffix = format!("_{session}.jsonl");
    let mut files = Vec::new();
    let mut dir = match tokio::fs::read_dir(sessions).await {
        Ok(dir) => dir,
        // The agent never ran.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    while let Some(entry) = dir.next_entry().await.map_err(io)? {
        if entry.file_name().to_string_lossy().ends_with(&suffix) {
            files.push(entry.path());
        }
    }
    files.sort();

    let mut steps = Vec::new();
    for file in files {
        let text = tokio::fs::read_to_string(&file).await.map_err(io)?;
        steps.extend(parse(&text, from, to));
    }
    Ok(steps)
}

fn parse(text: &str, from: Timestamp, to: Timestamp) -> Vec<TranscriptEntry> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .filter(|entry| entry.kind == "message" && (from..=to).contains(&entry.timestamp))
        .flat_map(|entry| {
            let at = entry.timestamp;
            entry
                .message
                .map(steps_of)
                .unwrap_or_default()
                .into_iter()
                .map(move |step| TranscriptEntry { at, step })
        })
        .collect()
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Timestamp,
    message: Option<Value>,
}

fn steps_of(message: Value) -> Vec<Step> {
    let blocks = blocks(&message["content"]);
    match message["role"].as_str() {
        Some("user") => vec![prompt(texts(&blocks))],
        Some("assistant") => blocks
            .iter()
            .filter_map(|block| match block["type"].as_str()? {
                "text" => Some(answer(block["text"].as_str()?)),
                "thinking" => Some(thinking(block["thinking"].as_str()?)),
                "toolCall" => Some(Step::ToolCall {
                    tool: block["name"].as_str()?.to_owned(),
                    arguments: block["arguments"].clone(),
                }),
                _ => None,
            })
            .collect(),
        Some("toolResult") => {
            let (text, truncated) = cut(texts(&blocks));
            vec![Step::ToolResult {
                tool: message["toolName"].as_str().unwrap_or_default().to_owned(),
                text,
                truncated,
                is_error: message["isError"].as_bool().unwrap_or(false),
            }]
        }
        // System messages carry the prompt and tool loadout, which the card shows.
        _ => Vec::new(),
    }
}

/// Content is a string or a list of blocks.
fn blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(text) => vec![serde_json::json!({ "type": "text", "text": text })],
        Value::Array(blocks) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn texts(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt(text: String) -> Step {
    let (text, truncated) = cut(text);
    Step::Prompt { text, truncated }
}

fn answer(text: &str) -> Step {
    let (text, truncated) = cut(text.to_owned());
    Step::Answer { text, truncated }
}

fn thinking(text: &str) -> Step {
    let (text, truncated) = cut(text.to_owned());
    Step::Thinking { text, truncated }
}

fn cut(mut text: String) -> (String, bool) {
    if text.len() <= MAX_TEXT {
        return (text, false);
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

fn io(error: std::io::Error) -> Error {
    Error::Internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> Timestamp {
        serde_json::from_value(Value::String(iso.to_owned())).unwrap()
    }

    const SESSION: &str = r#"{"type":"session","version":3,"id":"chain-x","timestamp":"2026-11-20T10:00:00.000Z","cwd":"/agent/work"}
{"type":"message","id":"a","parentId":null,"timestamp":"2026-11-20T10:00:01.000Z","message":{"role":"user","content":"an earlier Run","timestamp":0}}
{"type":"message","id":"b","parentId":"a","timestamp":"2026-11-20T10:05:00.000Z","message":{"role":"system","content":"","timestamp":0}}
{"type":"message","id":"c","parentId":"b","timestamp":"2026-11-20T10:05:01.000Z","message":{"role":"user","content":[{"type":"text","text":"You have mail"}],"timestamp":0}}
{"type":"message","id":"d","parentId":"c","timestamp":"2026-11-20T10:05:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"read it"},{"type":"text","text":"Reading."},{"type":"toolCall","id":"t1","name":"read_messages","arguments":{"session":"chain-x"}}],"timestamp":0}}
{"type":"model_change","id":"e","parentId":"d","timestamp":"2026-11-20T10:05:03.000Z","provider":"p","modelId":"m"}
not json
{"type":"message","id":"f","parentId":"d","timestamp":"2026-11-20T10:05:04.000Z","message":{"role":"toolResult","toolCallId":"t1","toolName":"read_messages","content":[{"type":"text","text":"hi"}],"isError":true,"timestamp":0}}
"#;

    #[test]
    fn a_run_is_the_steps_within_its_time() {
        let steps = parse(
            SESSION,
            at("2026-11-20T10:05:00Z"),
            at("2026-11-20T10:06:00Z"),
        );
        let kinds: Vec<Step> = steps.into_iter().map(|e| e.step).collect();
        assert_eq!(
            kinds,
            [
                Step::Prompt {
                    text: "You have mail".to_owned(),
                    truncated: false
                },
                Step::Thinking {
                    text: "read it".to_owned(),
                    truncated: false
                },
                Step::Answer {
                    text: "Reading.".to_owned(),
                    truncated: false
                },
                Step::ToolCall {
                    tool: "read_messages".to_owned(),
                    arguments: serde_json::json!({ "session": "chain-x" }),
                },
                Step::ToolResult {
                    tool: "read_messages".to_owned(),
                    text: "hi".to_owned(),
                    truncated: false,
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn long_texts_are_cut_on_a_char_boundary() {
        let (text, truncated) = cut("я".repeat(MAX_TEXT));
        assert!(truncated);
        assert!(text.len() <= MAX_TEXT);
        assert_eq!(cut("short".to_owned()), ("short".to_owned(), false));
    }
}
