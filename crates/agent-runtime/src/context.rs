//! The text Pi gets from Runtime: the Run context and message notifications (D80).

use control_plane_common::broker::Message;
use control_plane_common::{Alias, CorrelationId, SessionKey};

/// Appended to the agent's prompt with `--append-system-prompt` (D45).
///
/// `addressees` are the agent's allowed recipients with their titles, if they are agents.
pub fn run_context(
    agent: &Alias,
    session: SessionKey,
    correlation_id: CorrelationId,
    addressees: &[(Alias, Option<String>)],
) -> String {
    let mut text = format!(
        "You are {agent}.\n\
         Session: {session}. Chain: {correlation_id}.\n\
         You are notified of new messages without their text; \
         read them with the read_messages tool.\n"
    );
    if addressees.is_empty() {
        text.push_str("You may not message anyone.\n");
    } else {
        text.push_str("You may message:\n");
        for (alias, title) in addressees {
            match title {
                Some(title) => text.push_str(&format!("- {alias}: {title}\n")),
                None => text.push_str(&format!("- {alias}\n")),
            }
        }
    }
    text
}

/// A notification without the message text (D80).
pub fn notification(messages: &[Message]) -> String {
    match messages {
        [one] => format!("<new_message from=\"{}\" />", one.from),
        many => format!("<new_messages count=\"{}\" />", many.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_plane_common::{MessageId, TaskId, Timestamp};

    fn message(from: &str) -> Message {
        Message {
            id: MessageId::new(),
            from: from.parse().unwrap(),
            to: "coder".parse().unwrap(),
            body: "secret plans".to_owned(),
            subject: None,
            task_id: None,
            in_reply_to: None,
            correlation_id: CorrelationId::new(),
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn notification_never_carries_the_body() {
        let one = notification(&[message("manager")]);
        assert_eq!(one, r#"<new_message from="@manager" />"#);
        let many = notification(&[message("manager"), message("user")]);
        assert_eq!(many, r#"<new_messages count="2" />"#);
        assert!(!one.contains("secret") && !many.contains("secret"));
    }

    #[test]
    fn context_names_the_agent_session_and_recipients() {
        let task = TaskId::new();
        let chain = CorrelationId::new();
        let text = run_context(
            &"coder".parse().unwrap(),
            SessionKey::Task(task),
            chain,
            &[
                (
                    "manager".parse().unwrap(),
                    Some("Coordinates work".to_owned()),
                ),
                ("user".parse().unwrap(), None),
            ],
        );
        assert!(text.starts_with("You are @coder.\n"));
        assert!(text.contains(&format!("Session: task-{task}. Chain: {chain}.")));
        assert!(text.contains("- @manager: Coordinates work\n- @user\n"));
    }
}
