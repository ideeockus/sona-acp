-- seq is the delivery order; cursors point into it.
-- session is `task-<id>`, or `chain-<id>` for messages without a task (D78).
CREATE TABLE messages (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    id             TEXT NOT NULL UNIQUE,
    sender         TEXT NOT NULL,
    recipient      TEXT NOT NULL,
    body           TEXT NOT NULL,
    subject        TEXT,
    task_id        TEXT,
    in_reply_to    TEXT,
    correlation_id TEXT NOT NULL,
    session        TEXT NOT NULL,
    created_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX messages_inbox ON messages (recipient, session, seq);

-- One cursor per session of an agent (D49).
CREATE TABLE cursors (
    recipient TEXT NOT NULL,
    session   TEXT NOT NULL,
    up_to     INTEGER NOT NULL,
    PRIMARY KEY (recipient, session)
) STRICT;
