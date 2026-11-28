-- A thread is a chain now (D112): the operator's read cursor goes per chain. What was
-- read stays read: each chain's cursor starts at its newest message already read.
CREATE TABLE chain_reads (
    reader TEXT    NOT NULL,
    agent  TEXT    NOT NULL,
    chain  TEXT    NOT NULL,
    up_to  INTEGER NOT NULL,
    PRIMARY KEY (reader, agent, chain)
) STRICT;

INSERT INTO chain_reads (reader, agent, chain, up_to)
SELECT m.recipient, m.sender, m.correlation_id, MAX(m.seq)
FROM messages m
JOIN thread_reads r ON r.reader = m.recipient AND r.agent = m.sender AND r.session = m.session
WHERE m.seq <= r.up_to
GROUP BY m.recipient, m.sender, m.correlation_id;

DROP TABLE thread_reads;
