-- Browser sessions of the operator, opened with the credential (D103). Only hashes
-- are stored; a session ends at `expires_at` or on logout.
CREATE TABLE sessions (
    hash       BLOB    PRIMARY KEY,
    alias      TEXT    NOT NULL REFERENCES principals (alias),
    expires_at INTEGER NOT NULL
) STRICT;
