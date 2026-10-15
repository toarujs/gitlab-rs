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
# Requires: curl, cmp. Exit code 0 = all responses identical.
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

# compare <label> <path>
compare() {
  local label="$1" path="$2"
  local acode scode
  acode=$(curl -s -o "$tmp/a" -w '%{http_code}' "${AUTH[@]}" "$BASE$path")
  scode=$(curl -s -o "$tmp/s" -w '%{http_code}' "${AUTH[@]}" \
    -H 'X-Gitlab-Rs-Skip-Accel: 1' "$BASE$path")
  if [ "$acode" = "$scode" ] && cmp -s "$tmp/a" "$tmp/s"; then
    echo "OK   $label  http=$acode bytes=$(wc -c < "$tmp/a")"
  else
    echo "FAIL $label  accel=$acode skip=$scode"
    echo "     accel head: $(head -c 160 "$tmp/a")"
    echo "     skip  head: $(head -c 160 "$tmp/s")"
    fail=1
  fi
}

J="/api/v4/projects/$PROJECT_ID/jobs/$JOB_ID/trace"

# --- API trace contract (accelerated) ---
compare "trace full"            "$J"
compare "trace offset0 limit120" "$J?byte_offset=0&byte_limit=120"
compare "trace offset999999"    "$J?byte_offset=999999&byte_limit=100"
compare "trace limit0 (400)"    "$J?byte_limit=0"
compare "trace offset_neg (400)" "$J?byte_offset=-1"
compare "trace limit_big (400)" "$J?byte_limit=512001"

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
