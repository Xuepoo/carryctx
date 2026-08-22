# Commander Team Execution SOP

## Before Dispatch

- Read the current task graph, task scopes, design constraints, and repository status.
- Confirm every dependency is complete or intentionally informational.
- Group tightly coupled tasks and serialize overlapping scopes.
- Create or bind one worktree per independent code task.

## During Execution

- Assign or claim the task with the actual acting agent identity.
- Require incremental `progress` records and a checkpoint before handoff.
- Record blockers immediately and avoid silent retries after an agent exits.
- Keep implementation, review, and integration responsibilities separate.

## After Execution

- Review the diff, tests, public output, and scope before integration.
- Let Git perform merges and conflict resolution; CarryCtx records the result.
- Complete only tasks with verification evidence. Reopen or create follow-up tasks for unresolved findings.
- Run the repository quality gates before declaring a release wave complete.
