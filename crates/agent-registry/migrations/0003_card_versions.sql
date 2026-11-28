-- Every edit of a card is a version: who saved it, when, the whole card (D95).
-- `principals.card` stays the latest one, so reads need no history.
ALTER TABLE principals ADD COLUMN version INTEGER;

CREATE TABLE agent_versions (
    alias    TEXT    NOT NULL REFERENCES principals (alias),
    version  INTEGER NOT NULL,
    card     TEXT    NOT NULL,
    author   TEXT    NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (alias, version)
) STRICT;

-- Cards from before become version 1, saved by the operator now.
UPDATE principals SET version = 1 WHERE kind = 'agent';
INSERT INTO agent_versions (alias, version, card, author, saved_at)
SELECT alias, 1, card,
       (SELECT alias FROM principals WHERE kind = 'operator' LIMIT 1),
       CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM principals
WHERE kind = 'agent';
