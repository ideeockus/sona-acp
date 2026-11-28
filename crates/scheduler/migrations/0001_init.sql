-- Schedules of messages to agents (D107). `next_fire_at` is kept so a restart knows
-- which fires it missed; it is NULL while the schedule is disabled.
CREATE TABLE schedules (
    id            TEXT    PRIMARY KEY,
    agent         TEXT    NOT NULL,
    cron          TEXT    NOT NULL,
    timezone      TEXT    NOT NULL,
    subject       TEXT,
    body          TEXT    NOT NULL,
    enabled       INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    last_fired_at INTEGER,
    last_message  TEXT,
    next_fire_at  INTEGER
) STRICT;

CREATE INDEX schedules_due ON schedules (next_fire_at) WHERE next_fire_at IS NOT NULL;
