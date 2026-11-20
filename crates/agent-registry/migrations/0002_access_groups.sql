-- Cards grant groups of tools instead of single tools (D99). A tool grants its whole
-- group, so `read` alone becomes Read files with `ls`, `grep` and `find`. Tools of no
-- group, such as the always granted `read_messages`, are dropped.
UPDATE principals
SET card = json_set(
    json_remove(card, '$.allow_tools'),
    '$.access',
    json((
        SELECT json_group_array(access)
        FROM (
            SELECT DISTINCT
                CASE
                    WHEN value IN ('read', 'ls', 'grep', 'find') THEN 'read_files'
                    WHEN value IN ('write', 'edit') THEN 'write_files'
                    WHEN value = 'bash' THEN 'shell'
                    WHEN value IN ('task_create', 'task_get', 'task_update') THEN 'tasks'
                    WHEN value = 'send_message' THEN 'send_messages'
                END AS access
            FROM json_each(principals.card, '$.allow_tools')
        )
        WHERE access IS NOT NULL
    ))
)
WHERE kind = 'agent';
