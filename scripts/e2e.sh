#!/usr/bin/env bash
set -euo pipefail

# End-to-end smoke test for s4 against an S3-compatible endpoint.
#
# Required env vars:
#   S4_E2E_ENDPOINT   e.g. http://127.0.0.1:9000
#   S4_E2E_ACCESS_KEY access key id
#   S4_E2E_SECRET_KEY secret access key
#
# Optional env vars:
#   S4_E2E_REGION     default: us-east-1
#   S4_E2E_ALIAS      default: e2e
#   S4_E2E_PREFIX     default: s4-e2e
#   S4_E2E_PATH_STYLE default: 1 (1=true, 0=false)
#   S4_E2E_KEEP       default: 0 (1 keeps temp files)

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "[e2e] missing env var: $name" >&2
    exit 2
  fi
}

require_env S4_E2E_ENDPOINT
require_env S4_E2E_ACCESS_KEY
require_env S4_E2E_SECRET_KEY

REGION="${S4_E2E_REGION:-us-east-1}"
ALIAS="${S4_E2E_ALIAS:-e2e}"
PREFIX="${S4_E2E_PREFIX:-s4-e2e}"
PATH_STYLE="${S4_E2E_PATH_STYLE:-1}"
KEEP="${S4_E2E_KEEP:-0}"

TS="$(date +%s)"
RAND="$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
BUCKET="${PREFIX}-${TS}-${RAND}"
KEY="sample.txt"

WORKDIR="$(mktemp -d)"
CFG_DIR="$WORKDIR/config"
SRC_FILE="$WORKDIR/source.txt"
DST_FILE="$WORKDIR/downloaded.txt"
CAT_FILE="$WORKDIR/cat.txt"

cleanup() {
  local ec=$?
  if [[ "$KEEP" == "1" ]]; then
    echo "[e2e] keeping workdir: $WORKDIR"
    exit "$ec"
  fi

  set +e
  if [[ -f target/debug/s4 ]]; then
    target/debug/s4 -C "$CFG_DIR" rm "$ALIAS/$BUCKET/$KEY" >/dev/null 2>&1 || true
    target/debug/s4 -C "$CFG_DIR" rb "$ALIAS/$BUCKET" >/dev/null 2>&1 || true
    target/debug/s4 -C "$CFG_DIR" alias rm "$ALIAS" >/dev/null 2>&1 || true
  fi
  rm -rf "$WORKDIR"
  exit "$ec"
}
trap cleanup EXIT

run() {
  # Print command trace to stderr so stdout redirections capture only command output.
  echo "+ $*" >&2
  "$@"
}

echo "[e2e] building s4"
run cargo build

echo "[e2e] writing test payload"
printf 'hello-from-s4-e2e-%s\n' "$TS" > "$SRC_FILE"

echo "[e2e] setting alias"
if [[ "$PATH_STYLE" == "1" ]]; then
  run target/debug/s4 -C "$CFG_DIR" alias set "$ALIAS" "$S4_E2E_ENDPOINT" "$S4_E2E_ACCESS_KEY" "$S4_E2E_SECRET_KEY" --region "$REGION" --path-style
else
  run target/debug/s4 -C "$CFG_DIR" alias set "$ALIAS" "$S4_E2E_ENDPOINT" "$S4_E2E_ACCESS_KEY" "$S4_E2E_SECRET_KEY" --region "$REGION"
fi

run target/debug/s4 -C "$CFG_DIR" alias ls

echo "[e2e] list buckets"
run target/debug/s4 -C "$CFG_DIR" ls "$ALIAS" > "$WORKDIR/ls_buckets.out"

echo "[e2e] create bucket: $BUCKET"
run target/debug/s4 -C "$CFG_DIR" mb "$ALIAS/$BUCKET"

echo "[e2e] list objects in bucket (expected empty)"
run target/debug/s4 -C "$CFG_DIR" ls "$ALIAS/$BUCKET" > "$WORKDIR/ls_empty.out"

echo "[e2e] upload object"
run target/debug/s4 -C "$CFG_DIR" put "$SRC_FILE" "$ALIAS/$BUCKET/$KEY"

echo "[e2e] object stat"
run target/debug/s4 -C "$CFG_DIR" stat "$ALIAS/$BUCKET/$KEY" > "$WORKDIR/stat.out"

echo "[e2e] cat object"
run target/debug/s4 -C "$CFG_DIR" cat "$ALIAS/$BUCKET/$KEY" > "$CAT_FILE"

echo "[e2e] download object"
run target/debug/s4 -C "$CFG_DIR" get "$ALIAS/$BUCKET/$KEY" "$DST_FILE"

echo "[e2e] verify object content"
if ! cmp -s "$SRC_FILE" "$CAT_FILE"; then
  echo "[e2e] cat content mismatch" >&2
  exit 1
fi
if ! cmp -s "$SRC_FILE" "$DST_FILE"; then
  echo "[e2e] get content mismatch" >&2
  exit 1
fi

echo "[e2e] delete object"
run target/debug/s4 -C "$CFG_DIR" rm "$ALIAS/$BUCKET/$KEY"

echo "[e2e] delete bucket"
run target/debug/s4 -C "$CFG_DIR" rb "$ALIAS/$BUCKET"

echo "[e2e] remove alias"
run target/debug/s4 -C "$CFG_DIR" alias rm "$ALIAS"

echo "[e2e] success"
