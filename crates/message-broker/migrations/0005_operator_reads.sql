-- How far the operator has read each conversation (D102). Agents' cursors move only
-- with a finished Run (D49); the operator's moves when the conversation is opened.
CREATE TABLE operator_reads (
    reader TEXT    NOT NULL,
    agent  TEXT    NOT NULL,
    up_to  INTEGER NOT NULL,
    PRIMARY KEY (reader, agent)
) STRICT;

CREATE INDEX messages_by_sender ON messages (sender, recipient, seq);
