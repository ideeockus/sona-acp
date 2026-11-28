-- The status needs_owner is now needs_human. Statuses are stored JSON-encoded (D53).
UPDATE tasks SET status = 'needs_human' WHERE status = 'needs_owner';
UPDATE history SET change = replace(change, '"needs_owner"', '"needs_human"');
