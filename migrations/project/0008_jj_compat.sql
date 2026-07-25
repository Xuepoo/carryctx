-- Phase 2 of Jujutsu (jj) compatibility: record which VCS backend produced a
-- checkpoint's snapshot, and give jj-colocated repos an accurate "changed
-- files" list instead of a staged/unstaged split that jj's auto-snapshotting
-- makes unreliable (see carryctx-docs/plans/2026-07-25-jujutsu-compatibility.md).

ALTER TABLE checkpoints ADD COLUMN vcs_backend TEXT NOT NULL DEFAULT 'git';
ALTER TABLE checkpoints ADD COLUMN changed_files_json TEXT NOT NULL DEFAULT '[]';
