#!/usr/bin/env bash
# publish-snapshot.sh -- publish this repository's CarryCtx state to the
# orphan `carryctx-snapshots` branch for open, reviewable history.
#
# CarryCtx owns state semantics; Git owns transport. The script:
#   1. exports a ctxpack bundle (`carryctx export --pack-format dir`) into a
#      private staging directory;
#   2. redacts secret-shaped values in the STAGING copy only
#      (scripts/publish-snapshot-redact.py -- the local database is never
#      modified);
#   3. validates the redacted bundle with `carryctx import --dry-run` (writes
#      nothing to the database or Git);
#   4. commits one snapshot per invocation and pushes
#      HEAD:refs/heads/carryctx-snapshots to the chosen remote (unless
#      --no-push).
#
# Fail-closed: export, redaction, or validation failure aborts before any
# commit or push. `--dry-run` stops after step 3 in a temp directory and
# touches no Git state (the export audit event is appended to the local
# database by `carryctx export` itself, exactly as a normal export).
#
# Privacy: snapshots intentionally contain agent display names and absolute
# workspace paths (kept for debuggability) but never secret-shaped values.
# Redaction limits exposure; a secret that was already pushed anywhere still
# needs rotation at the source.
#
# Usage:
#   scripts/publish-snapshot.sh [--dry-run] [--no-push] [--branch NAME]
#                               [--worktree DIR] [--remote NAME] [--repo DIR]
#                               [--git-timeout SECS] [--keep-tmp]
#
# Env:
#   CARRYCTX_BIN   carryctx binary to run (default: carryctx on PATH)
#   GIT_TIMEOUT    timeout seconds for each git/carryctx/python call (default: 120)
set -euo pipefail

SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$SCRIPT_ROOT"
REDACTOR="$SCRIPT_ROOT/scripts/publish-snapshot-redact.py"

BRANCH="carryctx-snapshots"
REMOTE="origin"
GIT_TIMEOUT="${GIT_TIMEOUT:-120}"
CARRYCTX_BIN="${CARRYCTX_BIN:-carryctx}"
WORKTREE=""
DRY_RUN=0
PUSH=1
KEEP_TMP=0
REDACTIONS=0
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"

while [[ $# -gt 0 ]]; do
	case "$1" in
	--dry-run)
		DRY_RUN=1
		shift
		;;
	--no-push)
		PUSH=0
		shift
		;;
	--branch)
		BRANCH="${2:?--branch requires a name}"
		shift 2
		;;
	--branch=*)
		BRANCH="${1#--branch=}"
		shift
		;;
	--worktree)
		WORKTREE="${2:?--worktree requires a directory}"
		shift 2
		;;
	--worktree=*)
		WORKTREE="${1#--worktree=}"
		shift
		;;
	--remote)
		REMOTE="${2:?--remote requires a remote name}"
		shift 2
		;;
	--remote=*)
		REMOTE="${1#--remote=}"
		shift
		;;
	--repo)
		REPO_ROOT="${2:?--repo requires a directory}"
		shift 2
		;;
	--repo=*)
		REPO_ROOT="${1#--repo=}"
		shift
		;;
	--git-timeout)
		GIT_TIMEOUT="${2:?--git-timeout requires seconds}"
		shift 2
		;;
	--git-timeout=*)
		GIT_TIMEOUT="${1#--git-timeout=}"
		shift
		;;
	--keep-tmp)
		KEEP_TMP=1
		shift
		;;
	--help | -h)
		sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}"
		exit 0
		;;
	*)
		echo "publish-snapshot: FAIL: unknown flag $1 (see --help)" >&2
		exit 2
		;;
	esac
done

REPO_ROOT="$(cd "$REPO_ROOT" 2>/dev/null && pwd)" ||
	{
		echo "publish-snapshot: FAIL: --repo directory does not exist: $REPO_ROOT" >&2
		exit 2
	}
if [[ -z "$WORKTREE" ]]; then
	WORKTREE="${XDG_CACHE_HOME:-$HOME/.cache}/carryctx-snapshots/$(basename "$REPO_ROOT")"
fi

fail() {
	echo "publish-snapshot: FAIL: $1" >&2
	exit 1
}

log() {
	echo "publish-snapshot: $1"
}

have() { command -v "$1" >/dev/null 2>&1; }

have git || fail "git not on PATH"
have "$CARRYCTX_BIN" || fail "carryctx binary '$CARRYCTX_BIN' not found (set CARRYCTX_BIN)"
have python3 || fail "python3 not on PATH (needed by the redaction pass)"
have timeout || fail "timeout not on PATH"

# Fail early on a misconfigured remote: a publish that can never push must not
# create a snapshot branch first.
if [[ "$PUSH" == 1 && "$DRY_RUN" == 0 ]]; then
	timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" remote get-url "$REMOTE" >/dev/null 2>&1 ||
		fail "remote '$REMOTE' is not configured in $REPO_ROOT; add it or pass --no-push"
fi

# Worktree paths must be absolute for `git worktree list --porcelain` matching.
if [[ "$WORKTREE" != /* ]]; then
	WORKTREE="$PWD/$WORKTREE"
fi

TMP_ROOT=""
STAGING=""
cleanup() {
	if [[ "$KEEP_TMP" == 0 && -n "$TMP_ROOT" && -d "$TMP_ROOT" ]]; then
		rm -rf "$TMP_ROOT"
	elif [[ -n "$TMP_ROOT" ]]; then
		log "keeping tmp dir $TMP_ROOT (--keep-tmp)"
	fi
}
trap cleanup EXIT

redact_snapshot() {
	local dest="$1" redact_out
	log "redacting secret-shaped values in staging copy $dest (local DB untouched)"
	redact_out="$(timeout "$GIT_TIMEOUT" python3 "$REDACTOR" "$dest")" ||
		fail "secret redaction failed; nothing committed"
	log "$redact_out"
	REDACTIONS="$(printf '%s\n' "$redact_out" | sed -n 's/^REDACTIONS=//p')"
	REDACTIONS="${REDACTIONS:-0}"
}

validate_snapshot() {
	local dest="$1"
	log "validating redacted bundle (import --dry-run; writes nothing)"
	timeout "$GIT_TIMEOUT" "$CARRYCTX_BIN" import "$dest" \
		--mode replace --dry-run --format json --project "$REPO_ROOT" >/dev/null ||
		fail "bundle validation failed; nothing committed"
}

export_into() {
	local dest="$1"
	log "exporting ctxpack -> $dest"
	timeout "$GIT_TIMEOUT" "$CARRYCTX_BIN" export \
		--pack-format dir -o "$dest" --format json --project "$REPO_ROOT" >/dev/null ||
		fail "carryctx export failed; nothing committed"
	[[ -f "$dest/manifest.json" && -f "$dest/project.json" ]] ||
		fail "export incomplete (manifest.json/project.json missing)"
	redact_snapshot "$dest"
	validate_snapshot "$dest"
}

# Export + redact + validate into a private staging dir first. Git state is
# only touched after the bundle is complete and valid, so every failure path
# is fail-closed: no branch, no commit, no push.
stage_bundle() {
	TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/carryctx-snapshot.XXXXXX")"
	STAGING="$TMP_ROOT/ctxpack"
	mkdir -p "$STAGING"
	export_into "$STAGING"
}

worktree_registered() {
	timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" worktree list --porcelain |
		grep -Fxq "worktree $WORKTREE"
}

ensure_worktree() {
	if worktree_registered; then
		local current
		current="$(timeout "$GIT_TIMEOUT" git -C "$WORKTREE" symbolic-ref --short -q HEAD || true)"
		[[ "$current" == "$BRANCH" ]] ||
			fail "worktree $WORKTREE is on '${current:-detached}', expected '$BRANCH'"
		log "reusing snapshot worktree $WORKTREE"
		return 0
	fi
	if [[ -e "$WORKTREE" && -n "$(ls -A "$WORKTREE" 2>/dev/null)" ]]; then
		fail "$WORKTREE exists but is not a registered worktree of this repository; move it aside or pass --worktree"
	fi
	mkdir -p "$(dirname "$WORKTREE")"
	if timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$BRANCH"; then
		log "adding worktree $WORKTREE for existing branch $BRANCH"
		timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" worktree add -q "$WORKTREE" "$BRANCH" ||
			fail "git worktree add failed for branch $BRANCH"
	elif [[ "$PUSH" == 1 ]] &&
		timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" ls-remote --exit-code --heads "$REMOTE" "$BRANCH" >/dev/null 2>&1; then
		log "fetching existing snapshot branch $BRANCH from $REMOTE"
		timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" fetch -q "$REMOTE" "refs/heads/$BRANCH:refs/heads/$BRANCH" ||
			fail "failed to fetch $BRANCH from $REMOTE"
		timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" worktree add -q "$WORKTREE" "$BRANCH" ||
			fail "git worktree add failed for fetched branch $BRANCH"
	elif timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" worktree add -q --orphan -b "$BRANCH" "$WORKTREE" 2>/dev/null; then
		log "created orphan branch $BRANCH at $WORKTREE"
	else
		log "git worktree add --orphan unavailable; creating orphan branch $BRANCH at $WORKTREE (legacy path)"
		timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" worktree add -q --detach "$WORKTREE" ||
			fail "git worktree add --detach failed (the repository needs at least one commit)"
		timeout "$GIT_TIMEOUT" git -C "$WORKTREE" checkout -q --orphan "$BRANCH" ||
			fail "git checkout --orphan failed"
		timeout "$GIT_TIMEOUT" git -C "$WORKTREE" rm -rfq . >/dev/null 2>&1 || true
	fi
}

if [[ "$DRY_RUN" == 1 ]]; then
	stage_bundle
	log "dry-run PASS: export + redaction + validation succeeded (redactions=$REDACTIONS); no git state touched"
	exit 0
fi

stage_bundle
ensure_worktree
log "copying validated bundle -> $WORKTREE"
timeout "$GIT_TIMEOUT" cp -a "$STAGING/." "$WORKTREE/" || fail "failed to copy bundle into snapshot worktree"

timeout "$GIT_TIMEOUT" git -C "$WORKTREE" add -A || fail "git add failed in snapshot worktree"
if timeout "$GIT_TIMEOUT" git -C "$WORKTREE" diff --cached --quiet; then
	log "snapshot content unchanged; nothing to commit"
else
	SHORT_SHA="$(timeout "$GIT_TIMEOUT" git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
	timeout "$GIT_TIMEOUT" git -C "$WORKTREE" \
		-c user.name="${GIT_AUTHOR_NAME:-carryctx-publisher}" \
		-c user.email="${GIT_AUTHOR_EMAIL:-carryctx-publisher@users.noreply.github.com}" \
		commit -q --no-verify -m "chore(snapshot): $STAMP ($SHORT_SHA)" ||
		fail "snapshot commit failed"
	log "committed snapshot $STAMP (redactions=$REDACTIONS)"
fi

if [[ "$PUSH" == 1 ]]; then
	log "pushing HEAD:refs/heads/$BRANCH to $REMOTE"
	timeout "$GIT_TIMEOUT" git -C "$WORKTREE" push "$REMOTE" "HEAD:refs/heads/$BRANCH" ||
		fail "git push failed (network/auth/permissions?); the snapshot is committed locally"
	log "published $BRANCH"
else
	log "--no-push: snapshot committed to $BRANCH locally; push it yourself"
fi
