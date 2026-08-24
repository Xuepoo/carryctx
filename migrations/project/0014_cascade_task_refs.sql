-- CTX-0068 / issue #100: handoffs.task_id and
-- checkpoint_corrections.checkpoint_id were declared as plain (NO ACTION)
-- foreign keys, so pruning completed tasks either failed under
-- foreign_keys=ON or silently left orphaned rows behind under
-- foreign_keys=OFF. SQLite cannot alter a foreign key action in place, so
-- both child tables are rebuilt with ON DELETE CASCADE, copying every row
-- unchanged. The column layout is identical to migrations 0005/0006, so
-- `INSERT INTO archive.x SELECT * FROM main.x` archive copies stay
-- compatible across the migration.

PRAGMA legacy_alter_table = ON;

ALTER TABLE handoffs RENAME TO handoffs_rebuild_0014;

CREATE TABLE handoffs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  from_agent_id TEXT NOT NULL REFERENCES agents(id),
  to_agent_id TEXT REFERENCES agents(id),
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  session_id TEXT REFERENCES sessions(id),
  state TEXT NOT NULL CHECK(state IN ('pending', 'accepted', 'declined', 'expired', 'closed')),
  display_id TEXT NOT NULL UNIQUE,
  summary TEXT NOT NULL,
  context_json TEXT NOT NULL DEFAULT '{}',
  head TEXT,
  branch TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  accepted_at TEXT,
  declined_at TEXT,
  expires_at TEXT
);

INSERT INTO handoffs (
  id, project_id, from_agent_id, to_agent_id, task_id, session_id,
  state, display_id, summary, context_json, head, branch,
  created_at, updated_at, accepted_at, declined_at, expires_at
)
SELECT
  id, project_id, from_agent_id, to_agent_id, task_id, session_id,
  state, display_id, summary, context_json, head, branch,
  created_at, updated_at, accepted_at, declined_at, expires_at
FROM handoffs_rebuild_0014;

DROP TABLE handoffs_rebuild_0014;

CREATE INDEX IF NOT EXISTS handoffs_task_state_idx ON handoffs(task_id, state);
CREATE INDEX IF NOT EXISTS handoffs_to_agent_idx ON handoffs(to_agent_id, state);

ALTER TABLE checkpoint_corrections RENAME TO checkpoint_corrections_rebuild_0014;

CREATE TABLE checkpoint_corrections (
  id TEXT PRIMARY KEY,
  checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
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

INSERT INTO checkpoint_corrections (
  id, checkpoint_id, project_id, done_items_json, remaining_items_json,
  blockers_json, risks_json, next_steps_json, notes_json, reason, corrected_at
)
SELECT
  id, checkpoint_id, project_id, done_items_json, remaining_items_json,
  blockers_json, risks_json, next_steps_json, notes_json, reason, corrected_at
FROM checkpoint_corrections_rebuild_0014;

DROP TABLE checkpoint_corrections_rebuild_0014;

CREATE INDEX IF NOT EXISTS checkpoint_corrections_cp_idx
  ON checkpoint_corrections(checkpoint_id, corrected_at DESC);

PRAGMA legacy_alter_table = OFF;
