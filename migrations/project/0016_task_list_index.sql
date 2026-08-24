-- CTX-0072 / issue #105: the default task listing sorts by created_at DESC
-- within a project; without this index every list call paid a temp B-tree
-- sort over the whole project's rows (verified via EXPLAIN QUERY PLAN).
CREATE INDEX IF NOT EXISTS tasks_project_created_idx
    ON tasks(project_id, created_at DESC);
