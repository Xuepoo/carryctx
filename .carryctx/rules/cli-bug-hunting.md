# CLI Bug Hunting Rules & Verification Gates

## Exit Code & Output Separation Standards
1. **Exit Code 0**: Reserved strictly for successful executions.
2. **Exit Code 1 (General)**: User input error, invalid arguments, or expected domain failures.
3. **No Rust Panics**: Under no circumstances should unhandled `unwrap()` or `expect()` cause a process panic (`RUST_BACKTRACE=1`).

## Testing Guidelines
1. **JSON Output Validation**: Every JSON output must parse with zero schema errors.
2. **Database Cleanliness**: SQLite writes must be transaction-atomic; partial failures must roll back cleanly.
3. **Idempotent Maintenance Commands**: Running `carryctx doctor`, `carryctx project prune`, or `carryctx graph scan` multiple times in succession must not corrupt project state.
