-- Adapters are principals too (D71, D110): an external sender such as @github with a
-- credential, its title and who it may write to kept as JSON in `card`.
--
-- SQLite cannot change a CHECK in place, so the table is rebuilt inside the migration's
-- transaction. Foreign keys are deferred: dropping the old table counts its children as
-- orphans, and each row inserted into the new table under the old name counts them back.
PRAGMA defer_foreign_keys = ON;

CREATE TEMP TABLE principals_copy AS
SELECT alias, kind, card, version, disabled FROM principals;

CREATE TABLE principals_new (
    alias    TEXT PRIMARY KEY,
    kind     TEXT NOT NULL CHECK (kind IN ('operator', 'agent', 'adapter')),
    card     TEXT,
    version  INTEGER,
    disabled INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'operator') = (card IS NULL))
) STRICT;

DROP TABLE principals;
ALTER TABLE principals_new RENAME TO principals;

INSERT INTO principals (alias, kind, card, version, disabled)
SELECT alias, kind, card, version, disabled FROM principals_copy;

DROP TABLE principals_copy;
