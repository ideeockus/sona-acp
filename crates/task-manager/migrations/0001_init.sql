-- members and result are JSON: they are only ever read with the task.
CREATE TABLE tasks (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    context    TEXT NOT NULL,
    status     TEXT NOT NULL,
    creator    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    parent_id  TEXT REFERENCES tasks (id),
    assignee   TEXT,
    members    TEXT NOT NULL,
    deadline   INTEGER,
    result     TEXT
) STRICT;

-- Append-only (D53). change is a JSON-encoded Change.
CREATE TABLE history (
    seq     INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks (id),
    at      INTEGER NOT NULL,
    who     TEXT NOT NULL,
    change  TEXT NOT NULL
) STRICT;

CREATE INDEX history_task ON history (task_id, seq);
