-- Add first-class `rationale` support to decisions (see
-- https://github.com/Xuepoo/carryctx/issues/55) and stop deriving
-- `decisions.display_id` from a truncated ULID that quantises to a
-- 1024ms bucket and collides on rapid inserts (see
-- https://github.com/Xuepoo/carryctx/issues/54).
--
-- `rationale` was `NOT NULL` in 0006_collaboration but no CLI path could
-- ever set it, so every row silently stored ''. SQLite's ALTER TABLE
-- can't drop a NOT NULL constraint directly, so rebuild the table with
-- `rationale` nullable and no CHECK, preserving all existing rows
-- (their '' rationale becomes NULL, matching "not supplied").
--
-- decisions.display_id collisions (issue #54) are fixed at the
-- application layer: `decision add` now allocates from the `sequences`
-- table via `display_id_decision`, exactly like tasks and progress
-- items, instead of truncating a ULID. No schema change is needed for
-- that half of the fix.

CREATE TABLE decisions_new (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  task_id TEXT NOT NULL REFERENCES tasks(id),
  session_id TEXT REFERENCES sessions(id),
  display_id TEXT NOT NULL UNIQUE,
  title TEXT NOT NULL CHECK(length(trim(title)) > 0),
  context TEXT,
  decision_body TEXT,
  consequences TEXT,
  rationale TEXT,
  alternatives_json TEXT NOT NULL DEFAULT '[]',
  tags_json TEXT NOT NULL DEFAULT '[]',
  created_by_agent TEXT NOT NULL,
  created_by_session TEXT,
  superseded_by TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

INSERT INTO decisions_new
SELECT id, project_id, task_id, session_id, display_id, title, context,
       decision_body, consequences,
       CASE WHEN rationale = '' THEN NULL ELSE rationale END,
       alternatives_json, tags_json, created_by_agent, created_by_session,
       superseded_by, created_at, updated_at
FROM decisions;

DROP TABLE decisions;
ALTER TABLE decisions_new RENAME TO decisions;

CREATE INDEX IF NOT EXISTS decisions_task_created_idx ON decisions(task_id, created_at DESC);

-- Rebuild the FTS index (and its triggers) to include rationale, since
-- DROP TABLE decisions invalidated decisions_fts's rowid linkage and the
-- old triggers didn't reference the new column.
DROP TRIGGER IF EXISTS decisions_fts_ai;
DROP TRIGGER IF EXISTS decisions_fts_ad;
DROP TRIGGER IF EXISTS decisions_fts_au;
DROP TABLE IF EXISTS decisions_fts;

CREATE VIRTUAL TABLE decisions_fts USING fts5(
  title,
  context,
  decision_body,
  consequences,
  rationale
);

CREATE TRIGGER decisions_fts_ai AFTER INSERT ON decisions BEGIN
  INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences, rationale)
  VALUES (new.rowid, new.title, new.context, new.decision_body, new.consequences, new.rationale);
END;
CREATE TRIGGER decisions_fts_ad AFTER DELETE ON decisions BEGIN
  DELETE FROM decisions_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER decisions_fts_au AFTER UPDATE ON decisions BEGIN
  DELETE FROM decisions_fts WHERE rowid = old.rowid;
  INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences, rationale)
  VALUES (new.rowid, new.title, new.context, new.decision_body, new.consequences, new.rationale);
END;

INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences, rationale)
SELECT rowid, title, context, decision_body, consequences, rationale FROM decisions;
