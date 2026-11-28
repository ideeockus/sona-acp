-- Senders inside the control plane keep their alias to themselves: no agent may take
-- `scheduler` (D107). Retired aliases are never given, so it goes there.
INSERT OR IGNORE INTO retired_aliases (alias, retired_at) VALUES ('scheduler', 0);
