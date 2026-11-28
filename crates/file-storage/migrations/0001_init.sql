-- Files and who may read them (D106). The content lives in S3 under `<owner>/<id>`;
-- the name is only for people, so no name can reach another key.
CREATE TABLE files (
    id           TEXT    PRIMARY KEY,
    name         TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    content_type TEXT    NOT NULL,
    owner        TEXT    NOT NULL,
    created_at   INTEGER NOT NULL
) STRICT;

CREATE INDEX files_by_owner ON files (owner, created_at);

-- Readers besides the owner and the operator: recipients of a message it is attached to.
CREATE TABLE shares (
    file   TEXT NOT NULL REFERENCES files (id),
    reader TEXT NOT NULL,
    PRIMARY KEY (file, reader)
) STRICT;

CREATE INDEX shares_by_reader ON shares (reader);
