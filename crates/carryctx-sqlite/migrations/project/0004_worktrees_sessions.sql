CREATE TABLE IF NOT EXISTS worktrees (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  task_id TEXT REFERENCES tasks(id),
  normalized_path TEXT NOT NULL,
  git_common_dir TEXT NOT NULL,
  branch TEXT,
  head TEXT,
  git_snapshot_json TEXT,
  bound_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS worktrees_project_path_uq
  ON worktrees(project_id, normalized_path);

CREATE UNIQUE INDEX IF NOT EXISTS worktrees_active_task_uq
  ON worktrees(project_id, task_id)
  WHERE task_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  task_id TEXT REFERENCES tasks(id),
  worktree_id TEXT REFERENCES worktrees(id),
  state TEXT NOT NULL CHECK(state IN ('active', 'paused', 'ended', 'stale', 'abandoned')),
  provider TEXT NOT NULL,
  working_directory TEXT NOT NULL,
  branch TEXT,
  head TEXT,
  metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  started_at TEXT NOT NULL,
  last_activity_at TEXT NOT NULL,
  ended_at TEXT,
  summary TEXT,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_resolution_idx
  ON sessions(project_id, agent_id, worktree_id, state, last_activity_at DESC);
