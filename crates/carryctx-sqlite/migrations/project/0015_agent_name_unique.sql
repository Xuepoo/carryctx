-- CTX-0071 / issue #104: agent identity integrity.
--
-- Duplicate agent names make resolve-by-name attribute claims, events, and
-- session ownership to the wrong identity, which is fatal for multi-agent
-- orchestration. The UNIQUE(project_id, name) constraint was introduced by
-- 0002_work_model.sql and re-asserted after the agents table rebuilds in
-- 0012/0013. This migration idempotently re-creates the same index so any
-- database that lost the constraint through manual repair or a partial
-- restore converges back to the guaranteed invariant at schema version 15.
-- Application-level pre-checks in `agent register` / `agent rename` turn
-- violations into actionable errors suggesting a rename before this index
-- ever fires.

CREATE UNIQUE INDEX IF NOT EXISTS agents_project_name_uq
  ON agents(project_id, name);
