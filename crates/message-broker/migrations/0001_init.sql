-- seq is the delivery order; cursors point into it.
-- task_id is '' for messages without a task, so it can be part of a key.
CREATE TABLE messages (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    id             TEXT NOT NULL UNIQUE,
    sender         TEXT NOT NULL,
    recipient      TEXT NOT NULL,
    body           TEXT NOT NULL,
    subject        TEXT,
    task_id        TEXT NOT NULL,
    in_reply_to    TEXT,
    correlation_id TEXT NOT NULL,
    created_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX messages_inbox ON messages (recipient, task_id, seq);

-- One cursor per session: agent + task (D49).
CREATE TABLE cursors (
    recipient TEXT NOT NULL,
    task_id   TEXT NOT NULL,
    up_to     INTEGER NOT NULL,
    PRIMARY KEY (recipient, task_id)
) STRICT;
