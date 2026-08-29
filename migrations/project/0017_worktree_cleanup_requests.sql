-- CTX-0085 M1: durable outbox for worktree cleanup requests.
-- Snapshot fields (worktree_path/branch/task_id) are kept even after the
-- referenced worktree row is pruned, so a request remains actionable when
-- the worktree has already been removed from `worktrees`.

CREATE TABLE IF NOT EXISTS worktree_cleanup_requests (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  worktree_id TEXT REFERENCES worktrees(id) ON DELETE SET NULL,
  worktree_path TEXT NOT NULL CHECK (length(trim(worktree_path)) > 0),
  branch TEXT,
  task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  reason TEXT NOT NULL CHECK (reason IN ('task_completed', 'manual')),
  state TEXT NOT NULL CHECK (state IN ('pending', 'running', 'blocked', 'completed', 'failed', 'cancelled')),
  blocked_reason TEXT,
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  requested_at TEXT NOT NULL CHECK (length(trim(requested_at)) > 0),
  last_attempt_at TEXT,
  completed_at TEXT
);

CREATE INDEX IF NOT EXISTS worktree_cleanup_pending_idx
  ON worktree_cleanup_requests(project_id, state, requested_at);

CREATE INDEX IF NOT EXISTS worktree_cleanup_task_idx
  ON worktree_cleanup_requests(project_id, task_id);

CREATE UNIQUE INDEX IF NOT EXISTS worktree_cleanup_active_uq
  ON worktree_cleanup_requests(project_id, worktree_id)
  WHERE state IN ('pending', 'running', 'blocked');
