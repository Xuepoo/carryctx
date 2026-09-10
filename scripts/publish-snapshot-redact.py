#!/usr/bin/env python3
"""publish-snapshot-redact.py -- export-time secret redaction for ctxpack snapshots.

Ported (same author, MIT) from bitty-terminal/bitty-devtools
scripts/publish-ctxpack-redact.py so carryctx branch snapshots get the same
export-time redaction guarantees as the bitty workflow mirrors.

Usage: publish-snapshot-redact.py <snapshot-dir>

Rewrites every ``*.jsonl`` file under <snapshot-dir> IN PLACE, replacing
secret-shaped values with ``***REDACTED***`` while keeping field names (and,
inside free text, variable names) intact for debuggability.

What is redacted (fail-closed heuristics, JSONL-aware -- never blind sed):
  1. Key-name rule: any JSON object field whose name is secret-shaped (see
     is_secret_name: a _-separated or camelCase segment equals KEY / TOKEN /
     SECRET / PASSWORD / PAT, or the name contains GH_PAT, or it starts with
     CLOUDFLARE_ / AWS_) has a string value replaced wholesale.
     (AWS_/CLOUDFLARE_ prefixes intentionally over-match, e.g. AWS_REGION:
     a lost region string is cheaper than a leaked credential. A bare KEY
     segment such as keyboard-event handlers also redacts: fail-closed.)
  2. Free-text rule, applied to every remaining string value:
     a. ``NAME=value`` / ``NAME: value`` pairs (shell env dumps, YAML-ish
        prose, pasted kwargs) where NAME is secret-shaped per rule 1 -- the
        name is kept, the value is replaced (quoted values may contain
        spaces). Substring lookalikes do NOT match: ``monkey=``, ``keyboard=``,
        ``DispatchPriority:``, ``GOPATH_BIN=`` survive because KEY / PAT is
        not a standalone segment of those names.
     b. Standalone 40+-char ``[A-Za-z0-9_\\-+]`` runs (plus up to two ``=``
        padding chars) that look token-ish: exactly-40 hex runs are EXEMPT
        (git SHA-1s are ubiquitous in workflow prose and must survive;
        longer hex still redacts), single-class runs (all-lowercase
        test/branch names like ``soak_read_...``) are EXEMPT, and ``/`` and
        mid-run ``=`` are deliberately NOT in the run alphabet -- so absolute
        workspace paths and URLs (which the mirror intentionally publishes)
        are never mangled, and ``NAME=<token>`` never merges into one run
        that would eat the field name. A run must mix at least two of
        {lowercase, UPPERCASE, digits} or carry ``+``/``=`` base64 punctuation
        to count as token-ish. Slash-bearing tokens are still caught by
        rules 1 and 2a.

Nested JSON-encoded strings (carryctx ``payload_json`` / ``metadata_json``
columns) are decoded, redacted recursively, and re-encoded -- but only when
a redaction actually fired inside, so untouched rows stay byte-identical.

Scope and safety:
  - Operates ONLY on the snapshot directory argument (the staging/mirror
    copy or a --dry-run tmp dir). It NEVER touches the local carryctx DB;
    the DB keeps the original values and the next export redacts them again.
  - Row counts and file sets are unchanged, so manifest counts and the
    round-trip self-test still hold on redacted output.
  - Re-running is a no-op (``***REDACTED***`` carries no secret shape).
  - Files are replaced atomically (tmp + rename); rows that do not parse
    as JSON are left byte-identical and reported (the self-test then fails
    closed on them as malformed rows, as before).

Exit 0 on success (prints a per-run summary plus a ``REDACTIONS=<n>`` line
for the publish script to record in source.json). Exit 1 on structural
failure, 2 on usage error.
"""

import json
import os
import re
import sys

REDACTED = "***REDACTED***"

# Candidate ["]NAME["]=value | NAME: value | NAME = "quoted value", all on ONE
# line (separator whitespace is [ \t]* so `word:` never swallows the next
# line as its value). The name may itself be quoted (prose-embedded JSON
# snippets); the callback re-validates the name via is_secret_name, so this
# stays deliberately broad (any word around : or =) and non-secret pairs
# pass through byte-identical.
_ASSIGN_RE = re.compile(
    r"""(?P<nq>["']?)\b(?P<name>[A-Za-z_][A-Za-z0-9_.\-]*)(?P=nq)"""
    r"""[ \t]*(?P<sep>[:=])[ \t]*"""
    r"""(?:(?P<q>["'])(?P<qval>.*?)(?P=q)|(?P<val>[^\s"'`,;]+))""",
)

# `=` is padding-only, never a run joiner: otherwise NAME=<token> would merge
# into one run and eat the field name. Trailing `={0,2}` still catches
# base64 padding without gluing across `=`.
_RUN_RE = re.compile(r"[A-Za-z0-9_\-+]{40,}={0,2}")
_SHA1_RE = re.compile(r"[0-9a-fA-F]{40}\Z")

_SEGMENT_RE = re.compile(r"[A-Z]+(?![a-z])|[A-Z][a-z0-9]*|[a-z]+|[0-9]+")
_SECRET_SEGMENTS = frozenset(["KEY", "TOKEN", "SECRET", "PASSWORD", "PAT"])


def _segments(name):
    """Split a field/variable name into UPPER-cased word segments."""
    found = []
    for part in re.split(r"[_\-.]+", name):
        found.extend(_SEGMENT_RE.findall(part))
    return [seg.upper() for seg in found]


def is_secret_name(name):
    """True when a field/variable name is secret-shaped (spec list).

    Segment-based, so ``monkey``, ``keyboard``, ``tokenizer``,
    ``DispatchPriority`` and ``GOPATH_BIN`` do NOT match while ``api_key``,
    ``OPENAI_API_KEY``, ``gh_pat`` and ``handle_key_event`` do.
    """
    upper = name.upper()
    if "GH_PAT" in upper:
        return True
    if upper.startswith("CLOUDFLARE_") or upper.startswith("AWS_"):
        return True
    return any(seg in _SECRET_SEGMENTS for seg in _segments(name))


def _looks_tokenish(run):
    """True when a 40+ run mixes classes like a real token (not a slug)."""
    if "+" in run or run.endswith("="):
        return True
    classes = 0
    if re.search(r"[a-z]", run):
        classes += 1
    if re.search(r"[A-Z]", run):
        classes += 1
    if re.search(r"[0-9]", run):
        classes += 1
    return classes >= 2


def _already_redacted(value):
    return value == REDACTED or value.startswith("***")


def redact_text(text):
    """Apply free-text rules to one string. Returns (new_text, count)."""
    count = 0

    # NOTE: the span rebuild below preserves the exact original separator
    # whitespace (quoted values may contain spaces; bare values end at
    # whitespace or quote/comma/semicolon boundaries). Names that are not
    # secret-shaped pass through byte-identical (no count).
    def _assign_simple(match):
        nonlocal count
        if not is_secret_name(match.group("name")):
            return match.group(0)
        qval = match.group("qval")
        if qval is not None:
            if not qval or _already_redacted(qval):
                return match.group(0)
            count += 1
            start, end = match.span("qval")
            return match.string[match.start():start] + REDACTED + match.string[end:match.end()]
        value = match.group("val")
        if not value or _already_redacted(value):
            return match.group(0)
        count += 1
        start, end = match.span("val")
        return match.string[match.start():start] + REDACTED + match.string[end:match.end()]

    text = _ASSIGN_RE.sub(_assign_simple, text)

    def _run(match):
        nonlocal count
        run = match.group(0)
        if _already_redacted(run):
            return run
        if _SHA1_RE.match(run):
            return run  # git SHA-1 exemption (documented above)
        if not _looks_tokenish(run):
            return run  # single-class slug (branch/test/flag name), not a token
        count += 1
        return REDACTED

    text = _RUN_RE.sub(_run, text)
    return text, count


def redact_value(node):
    """Recursively redact decoded JSON. Returns (new_node, count)."""
    if isinstance(node, dict):
        count = 0
        out = {}
        for key, value in node.items():
            if isinstance(key, str) and isinstance(value, str):
                if is_secret_name(key):
                    if value and not _already_redacted(value):
                        out[key] = REDACTED
                        count += 1
                    else:
                        out[key] = value
                    continue
            new_value, sub = redact_value(value)
            out[key] = new_value
            count += sub
        return out, count
    if isinstance(node, list):
        count = 0
        out = []
        for item in node:
            new_item, sub = redact_value(item)
            out.append(new_item)
            count += sub
        return out, count
    if isinstance(node, str):
        stripped = node.strip()
        if len(stripped) >= 2 and stripped[0] in "{[":
            try:
                decoded = json.loads(stripped)
            except ValueError:
                decoded = None
            if isinstance(decoded, (dict, list)):
                new_decoded, sub = redact_value(decoded)
                if sub:
                    return (
                        json.dumps(new_decoded, ensure_ascii=False, separators=(",", ":")),
                        sub,
                    )
                return node, 0
        return redact_text(node)
    return node, 0


def redact_file(path):
    """Redact one .jsonl file atomically. Returns (rows, count, skipped)."""
    with open(path, "r", encoding="utf-8") as handle:
        raw_lines = handle.read().splitlines()
    out_lines = []
    rows = 0
    count = 0
    skipped = 0
    changed = False
    for line in raw_lines:
        if not line.strip():
            out_lines.append(line)
            continue
        try:
            row = json.loads(line)
        except ValueError:
            skipped += 1
            out_lines.append(line)  # byte-identical; self-test fails closed
            continue
        rows += 1
        new_row, sub = redact_value(row)
        count += sub
        if sub:
            out_lines.append(json.dumps(new_row, ensure_ascii=False))
            changed = True
        else:
            out_lines.append(line)  # byte-identical fast path
    if changed:
        tmp_path = path + ".redact-tmp"
        with open(tmp_path, "w", encoding="utf-8") as handle:
            handle.write("\n".join(out_lines))
            if out_lines:
                handle.write("\n")
        os.replace(tmp_path, path)
    return rows, count, skipped


def main(argv):
    if len(argv) != 2 or argv[1] in ("-h", "--help"):
        sys.stderr.write("usage: publish-snapshot-redact.py <snapshot-dir>\n")
        return 2
    snap = argv[1]
    if not os.path.isdir(snap):
        sys.stderr.write("publish-snapshot-redact: FAIL: not a directory: %s\n" % snap)
        return 1
    files = sorted(
        name
        for name in os.listdir(snap)
        if name.endswith(".jsonl")
        and os.path.isfile(os.path.join(snap, name))
        and not os.path.islink(os.path.join(snap, name))
    )
    if not files:
        sys.stderr.write(
            "publish-snapshot-redact: FAIL: no *.jsonl files in %s (not a ctxpack dir?)\n" % snap
        )
        return 1
    total_rows = 0
    total_count = 0
    total_skipped = 0
    for name in files:
        rows, count, skipped = redact_file(os.path.join(snap, name))
        total_rows += rows
        total_count += count
        total_skipped += skipped
        if count or skipped:
            print("  %-28s rows=%d redactions=%d nonjson_skipped=%d" % (name, rows, count, skipped))
    print(
        "publish-snapshot-redact: %d files, %d rows, %d replacements, %d non-JSON lines left untouched"
        % (len(files), total_rows, total_count, total_skipped)
    )
    print("REDACTIONS=%d" % total_count)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
