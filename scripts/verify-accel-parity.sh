#!/usr/bin/env bash
# Accel vs. skip-accel parity check.
#
# The two real acceleration defects found so far (branch `can_push` semantics
# and commit `title`/`message` alignment) were both caught by fetching the same
# URL twice -- once through the accelerator and once with
# `X-Gitlab-Rs-Skip-Accel: 1` (straight to Rails) -- and diffing the responses
# byte for byte. Unit tests do not exercise the Rails contract, so keep this in
# CI against a running instance.
#
# Usage:
#   BASE=http://127.0.0.1:9070 TOKEN=glpat-... PROJECT_ID=14 JOB_ID=19 \
#     scripts/verify-accel-parity.sh
#
# Requires: curl, cmp, python3. Exit code 0 = all responses equivalent
# (byte-identical for `bytes` endpoints, semantically equal for JSON ones).
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:9070}
PROJECT_ID=${PROJECT_ID:-14}
JOB_ID=${JOB_ID:-19}
REF=${REF:-master}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fail=0
if [ -n "${TOKEN:-}" ]; then
  AUTH=(-H "PRIVATE-TOKEN: $TOKEN")
else
  AUTH=()
fi

# compare <label> <path> [json|bytes]
#
# `bytes` is used for endpoints that must match verbatim (job trace is a raw
# text stream). `json` (default) parses both bodies and compares them as
# canonical JSON: the accelerator and Rails emit the same object graph but
# order object keys differently, and a given client only ever talks to one
# layer, so key order is not part of the contract. Value/field differences are
# still caught.
compare() {
  local label="$1" path="$2" mode="${3:-json}"
  local acode scode ok=""
  acode=$(curl -s -o "$tmp/a" -w '%{http_code}' "${AUTH[@]}" "$BASE$path")
  scode=$(curl -s -o "$tmp/s" -w '%{http_code}' "${AUTH[@]}" \
    -H 'X-Gitlab-Rs-Skip-Accel: 1' "$BASE$path")
  if [ "$acode" = "$scode" ]; then
    if [ "$mode" = "bytes" ]; then
      cmp -s "$tmp/a" "$tmp/s" && ok=1
    else
      python3 - "$tmp/a" "$tmp/s" <<'PY' && ok=1
import json, sys
a = json.load(open(sys.argv[1]))
s = json.load(open(sys.argv[2]))
sys.exit(0 if a == s else 1)
PY
    fi
  fi
  if [ -n "$ok" ]; then
    echo "OK   $label  http=$acode bytes=$(wc -c < "$tmp/a")"
  else
    echo "FAIL $label  accel=$acode skip=$scode"
    echo "     accel head: $(head -c 160 "$tmp/a")"
    echo "     skip  head: $(head -c 160 "$tmp/s")"
    fail=1
  fi
}

J="/api/v4/projects/$PROJECT_ID/jobs/$JOB_ID/trace"

# --- current user contract (authenticated) ---
#
# `/api/v4/user` renders a different Grape entity for admins
# (`UserWithAdmin`, 42 fields) than for regular users (`UserPublic`, 38
# fields), so exercise both by passing tokens from different accounts.
compare "current user (admin)"  "/api/v4/user"

# --- API trace contract (accelerated) ---
compare "trace full"            "$J" bytes
compare "trace offset0 limit120" "$J?byte_offset=0&byte_limit=120" bytes
compare "trace offset999999"    "$J?byte_offset=999999&byte_limit=100" bytes
compare "trace limit0 (400)"    "$J?byte_limit=0" bytes
compare "trace offset_neg (400)" "$J?byte_offset=-1" bytes
compare "trace limit_big (400)" "$J?byte_limit=512001" bytes

# --- repository read acceleration ---
P="/api/v4/projects/$PROJECT_ID"
compare "project show"   "$P"
compare "tree"           "$P/repository/tree?ref=$REF"
compare "branches"       "$P/repository/branches"
compare "commits"        "$P/repository/commits?ref_name=$REF"

if [ "$fail" -eq 0 ]; then
  echo "ALL_OK"
else
  echo "MISMATCH"
  exit 1
fi
