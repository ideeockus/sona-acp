-- Why the system stopped a chain, such as a safeguard; NULL when the operator did (D100).
ALTER TABLE stopped_chains ADD COLUMN reason TEXT;

-- When each chain was last resumed: its limits count from there (D100).
CREATE TABLE chain_resumes (
    correlation_id TEXT PRIMARY KEY,
    resumed_at     INTEGER NOT NULL
) STRICT;
