CREATE TABLE IF NOT EXISTS scopes (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  task_id TEXT NOT NULL REFERENCES tasks(id),
  pattern TEXT NOT NULL CHECK(length(trim(pattern)) > 0),
  kind TEXT NOT NULL CHECK(kind IN ('include', 'exclude')),
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS scopes_task_idx ON scopes(task_id, kind);

CREATE TABLE IF NOT EXISTS decisions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  task_id TEXT NOT NULL REFERENCES tasks(id),
  session_id TEXT REFERENCES sessions(id),
  display_id TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL CHECK(length(trim(title)) > 0),
  context TEXT,
  decision_body TEXT,
  consequences TEXT,
  rationale TEXT NOT NULL,
  alternatives_json TEXT NOT NULL DEFAULT '[]',
  tags_json TEXT NOT NULL DEFAULT '[]',
  created_by_agent TEXT NOT NULL,
  created_by_session TEXT,
  superseded_by TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS decisions_task_created_idx ON decisions(task_id, created_at DESC);

CREATE TABLE IF NOT EXISTS handoffs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  from_agent_id TEXT NOT NULL REFERENCES agents(id),
  to_agent_id TEXT REFERENCES agents(id),
  task_id TEXT NOT NULL REFERENCES tasks(id),
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

CREATE INDEX IF NOT EXISTS handoffs_task_state_idx ON handoffs(task_id, state);
CREATE INDEX IF NOT EXISTS handoffs_to_agent_idx ON handoffs(to_agent_id, state);
