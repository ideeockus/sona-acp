-- Chains the operator stopped: their messages are kept but delivered to nobody (D27).
CREATE TABLE stopped_chains (
    correlation_id TEXT PRIMARY KEY,
    stopped_at     INTEGER NOT NULL
) STRICT;
