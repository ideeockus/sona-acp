-- Every Run that started (D97). Usage grows with each model answer; the
-- transcript itself stays in the Pi session file.
CREATE TABLE runs (
    id             TEXT PRIMARY KEY,
    agent          TEXT NOT NULL,
    session        TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    started_at     INTEGER NOT NULL,
    finished_at    INTEGER,
    outcome        TEXT NOT NULL,
    reason         TEXT,
    tokens_in      INTEGER NOT NULL DEFAULT 0,
    tokens_out     INTEGER NOT NULL DEFAULT 0,
    cost_micros    INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE INDEX runs_by_agent ON runs (agent, started_at);
CREATE INDEX runs_by_chain ON runs (correlation_id, started_at);
