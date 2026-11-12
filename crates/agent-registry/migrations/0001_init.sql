-- Agents and the operator share one alias namespace (D3).
CREATE TABLE principals (
    alias TEXT PRIMARY KEY,
    kind  TEXT NOT NULL CHECK (kind IN ('operator', 'agent')),
    card  TEXT,
    CHECK ((kind = 'agent') = (card IS NOT NULL))
) STRICT;

-- Only hashes are stored. A Run token has run_id and correlation_id, others have neither.
CREATE TABLE credentials (
    hash           BLOB PRIMARY KEY,
    alias          TEXT NOT NULL REFERENCES principals (alias),
    run_id         TEXT,
    correlation_id TEXT,
    CHECK ((run_id IS NULL) = (correlation_id IS NULL))
) STRICT;

CREATE INDEX credentials_run_id ON credentials (run_id) WHERE run_id IS NOT NULL;
