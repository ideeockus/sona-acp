-- Failed Runs of a session in a row, and whether its delivery stopped (D77).
-- A successful Run deletes the row.
CREATE TABLE failures (
    recipient  TEXT NOT NULL,
    session    TEXT NOT NULL,
    count      INTEGER NOT NULL,
    last_error TEXT NOT NULL,
    stuck      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (recipient, session)
) STRICT;
