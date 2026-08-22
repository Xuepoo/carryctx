# Team Execution Rules

1. CarryCtx records durable project state; the external harness owns process execution and parallelism.
2. One code task uses one Git worktree unless the commander explicitly groups tightly coupled changes.
3. A task must declare file or subsystem scope before parallel dispatch. Treat overlapping scopes as a serialization point.
4. Every subagent writes an initial plan, at least one progress update, and a completion checkpoint or blocker note.
5. Reclaim abandoned work only through an audited, authorized path. Never rely on another agent's release permission bug.
6. A completed task requires focused tests, formatting, linting, and reviewer evidence; full repository gates run before the release wave.
