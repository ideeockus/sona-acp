-- The skill library (D109). The content of each version lives under
-- data_dir/skills/<name>/<version>/ and never changes once written.
CREATE TABLE skills (
    name        TEXT    PRIMARY KEY,
    description TEXT    NOT NULL,
    source      TEXT    NOT NULL,
    version     TEXT    NOT NULL,
    enabled     INTEGER NOT NULL,
    added_by    TEXT    NOT NULL,
    added_at    INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;
