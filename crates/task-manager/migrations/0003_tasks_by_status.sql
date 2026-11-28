-- The operator lists tasks by status, newest first (D102).
CREATE INDEX tasks_by_status ON tasks (status, created_at);
