-- A task's space (D56, D111): files its participants share, its result among them.
-- Who takes part is asked of Task Manager at each access.
ALTER TABLE files ADD COLUMN task_id TEXT;

CREATE INDEX files_by_task ON files (task_id, created_at) WHERE task_id IS NOT NULL;
