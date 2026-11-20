-- The version of the card a Run ran with (D95). Runs from before ran with cards that
-- became version 1.
ALTER TABLE runs ADD COLUMN card_version INTEGER NOT NULL DEFAULT 1;
