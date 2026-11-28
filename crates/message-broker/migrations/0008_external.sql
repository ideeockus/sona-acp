-- A message from an adapter is text from outside (D110): agents get it marked, as data
-- and never as instructions.
ALTER TABLE messages ADD COLUMN external INTEGER NOT NULL DEFAULT 0;
