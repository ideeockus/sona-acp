use super::*;

fn parse(line: &str) -> Event {
    serde_json::from_str(line).unwrap()
}

#[test]
fn commands_match_pi_format() {
    let prompt = Command::Prompt {
        message: "hi".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&prompt).unwrap(),
        r#"{"type":"prompt","message":"hi"}"#
    );
    assert_eq!(
        serde_json::to_string(&Command::Abort).unwrap(),
        r#"{"type":"abort"}"#
    );
}

#[test]
fn parses_the_events_runtime_uses() {
    // Samples from Pi's JSON event stream docs, including fields we ignore.
    assert_eq!(
        parse(
            r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{"command":"ls"}}"#
        ),
        Event::ToolExecutionStart {
            tool_call_id: "c1".to_owned(),
            tool_name: "bash".to_owned()
        }
    );
    assert_eq!(
        parse(
            r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[]},"isError":true}"#
        ),
        Event::ToolExecutionEnd {
            tool_call_id: "c1".to_owned(),
            tool_name: "bash".to_owned(),
            is_error: true
        }
    );
    assert_eq!(parse(r#"{"type":"agent_settled"}"#), Event::AgentSettled);
    assert_eq!(
        parse(r#"{"id":"p1","type":"response","command":"prompt","success":false,"error":"busy"}"#),
        Event::Response {
            command: "prompt".to_owned(),
            success: false,
            error: Some("busy".to_owned())
        }
    );
}

#[test]
fn unknown_events_do_not_break_parsing() {
    assert_eq!(
        parse(r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta"}}"#),
        Event::Other
    );
}

#[tokio::test]
async fn reader_splits_on_line_feeds_only() {
    // A U+2028 inside a string, a CRLF line end and an empty line.
    let stream = "{\"type\":\"response\",\"command\":\"a\u{2028}b\",\"success\":true}\r\n\n{\"type\":\"agent_settled\"}\n";
    let mut reader = EventReader::new(stream.as_bytes());
    assert_eq!(
        reader.next().await.unwrap(),
        Some(Event::Response {
            command: "a\u{2028}b".to_owned(),
            success: true,
            error: None
        })
    );
    assert_eq!(reader.next().await.unwrap(), Some(Event::AgentSettled));
    assert_eq!(reader.next().await.unwrap(), None);
}

#[tokio::test]
async fn garbage_on_stdout_is_an_error() {
    let mut reader = EventReader::new("Debugger attached.\n".as_bytes());
    assert!(reader.next().await.is_err());
}

#[tokio::test]
async fn writer_sends_one_line_per_command() {
    let mut buffer = Vec::new();
    let mut writer = CommandWriter::new(&mut buffer);
    writer
        .send(&Command::Steer {
            message: "also".to_owned(),
        })
        .await
        .unwrap();
    writer.send(&Command::Abort).await.unwrap();
    assert_eq!(
        String::from_utf8(buffer).unwrap(),
        "{\"type\":\"steer\",\"message\":\"also\"}\n{\"type\":\"abort\"}\n"
    );
}

#[tokio::test]
async fn reader_survives_cancellation_mid_line() {
    use tokio::io::AsyncWriteExt;
    use tokio::time::{Duration, timeout};

    let (mut pi, stdout) = tokio::io::duplex(64);
    let mut reader = EventReader::new(stdout);

    pi.write_all(br#"{"type":"agent_"#).await.unwrap();
    // The read is cancelled while half a line is buffered.
    assert!(
        timeout(Duration::from_millis(20), reader.next())
            .await
            .is_err()
    );

    pi.write_all(b"settled\"}\n").await.unwrap();
    assert_eq!(reader.next().await.unwrap(), Some(Event::AgentSettled));
}

#[tokio::test]
async fn last_line_without_line_feed_is_read() {
    let mut reader = EventReader::new(r#"{"type":"agent_settled"}"#.as_bytes());
    assert_eq!(reader.next().await.unwrap(), Some(Event::AgentSettled));
    assert_eq!(reader.next().await.unwrap(), None);
}
