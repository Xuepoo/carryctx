CREATE TABLE IF NOT EXISTS checkpoints (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  task_id TEXT NOT NULL REFERENCES tasks(id),
  session_id TEXT REFERENCES sessions(id),
  worktree_id TEXT REFERENCES worktrees(id),
  agent_id TEXT,
  branch TEXT,
  head TEXT,
  dirty INTEGER NOT NULL DEFAULT 0,
  staged_files_json TEXT NOT NULL DEFAULT '[]',
  modified_files_json TEXT NOT NULL DEFAULT '[]',
  deleted_files_json TEXT NOT NULL DEFAULT '[]',
  renamed_files_json TEXT NOT NULL DEFAULT '[]',
  untracked_files_json TEXT NOT NULL DEFAULT '[]',
  diff_files INTEGER,
  diff_insertions INTEGER,
  diff_deletions INTEGER,
  done_items_json TEXT NOT NULL DEFAULT '[]',
  remaining_items_json TEXT NOT NULL DEFAULT '[]',
  blockers_json TEXT NOT NULL DEFAULT '[]',
  risks_json TEXT NOT NULL DEFAULT '[]',
  next_steps_json TEXT NOT NULL DEFAULT '[]',
  notes_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS checkpoints_task_created_idx
  ON checkpoints(task_id, created_at DESC);

CREATE TABLE IF NOT EXISTS checkpoint_corrections (
  id TEXT PRIMARY KEY,
  checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id),
  project_id TEXT NOT NULL REFERENCES projects(id),
  done_items_json TEXT,
  remaining_items_json TEXT,
  blockers_json TEXT,
  risks_json TEXT,
  next_steps_json TEXT,
  notes_json TEXT,
  reason TEXT,
  corrected_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS checkpoint_corrections_cp_idx
  ON checkpoint_corrections(checkpoint_id, corrected_at DESC);
