-- What agents chose to remember (D31, D108), with the Run that wrote each record.
CREATE TABLE records (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    id             TEXT    NOT NULL UNIQUE,
    agent          TEXT    NOT NULL,
    text           TEXT    NOT NULL,
    run_id         TEXT,
    correlation_id TEXT,
    created_at     INTEGER NOT NULL
) STRICT;

CREATE INDEX records_by_agent ON records (agent, seq);

-- Search by substring of three characters or more, in any language (D108).
CREATE VIRTUAL TABLE records_text USING fts5 (
    text,
    content = 'records',
    content_rowid = 'seq',
    tokenize = 'trigram'
);

CREATE TRIGGER records_added AFTER INSERT ON records BEGIN
    INSERT INTO records_text (rowid, text) VALUES (new.seq, new.text);
END;

CREATE TRIGGER records_deleted AFTER DELETE ON records BEGIN
    INSERT INTO records_text (records_text, rowid, text) VALUES ('delete', old.seq, old.text);
END;
