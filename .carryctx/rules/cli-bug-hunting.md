# CLI Bug Hunting Rules & Verification Gates

## Exit Code & Output Separation Standards

1. **Exit Code 0**
2. **Exit Code 1 (General)**: User input error, invalid arguments, or expected domain failures.

## Testing Guidelines

1. **JSON Output Validation**: Every JSON output must parse with zero schema errors.
2. **Database Cleanliness**: SQLite writes must be transaction-atomic; partial failures must roll back cleanly.
3. **Idempotent Maintenance Commands**: Running `carryctx doctor`, `carryctx project prune`, or `carryctx graph scan` multiple times in succession must not corrupt project state.
