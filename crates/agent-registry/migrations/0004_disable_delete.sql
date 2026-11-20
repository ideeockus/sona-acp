-- A disabled agent is not woken and takes no messages (D96).
ALTER TABLE principals ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;

-- Aliases of deleted agents: never given again, so no new agent inherits another's
-- messages, sessions and place in `allow_message_to` (D96).
CREATE TABLE retired_aliases (
    alias      TEXT PRIMARY KEY,
    retired_at INTEGER NOT NULL
) STRICT;
