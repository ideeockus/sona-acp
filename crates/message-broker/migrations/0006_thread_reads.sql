-- The operator reads each conversation with an agent on its own (D105): opening one
-- leaves the others unread. What was read before stays read in every one of them.
CREATE TABLE thread_reads (
    reader  TEXT    NOT NULL,
    agent   TEXT    NOT NULL,
    session TEXT    NOT NULL,
    up_to   INTEGER NOT NULL,
    PRIMARY KEY (reader, agent, session)
) STRICT;

INSERT INTO thread_reads (reader, agent, session, up_to)
SELECT DISTINCT r.reader, r.agent, m.session, r.up_to
FROM operator_reads r
JOIN messages m
  ON (m.sender = r.agent AND m.recipient = r.reader)
  OR (m.sender = r.reader AND m.recipient = r.agent);

DROP TABLE operator_reads;
