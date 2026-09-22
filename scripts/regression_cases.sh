#!/usr/bin/env bash
set -uo pipefail

# Regression checks for bugs fixed in s4, one check per bug.
# Runs against a live S3-compatible endpoint and exits non-zero on any failure.
#
# Required env vars:
#   S4_E2E_ENDPOINT    e.g. http://127.0.0.1:9000
#   S4_E2E_ACCESS_KEY  access key id
#   S4_E2E_SECRET_KEY  secret access key
#
# Optional env vars:
#   S4_E2E_REGION      default: us-east-1
#   S4_E2E_ENDPOINT2, S4_E2E_ACCESS_KEY2, S4_E2E_SECRET_KEY2
#                      a second endpoint (another server or account) for
#                      cross-endpoint cp/sync checks; skipped when unset
#   S4_BIN             prebuilt binary; default: `cargo build` + target/debug/s4

: "${S4_E2E_ENDPOINT:?S4_E2E_ENDPOINT is required}"
: "${S4_E2E_ACCESS_KEY:?S4_E2E_ACCESS_KEY is required}"
: "${S4_E2E_SECRET_KEY:?S4_E2E_SECRET_KEY is required}"
REGION="${S4_E2E_REGION:-us-east-1}"

if [[ -z "${S4_BIN:-}" ]]; then
  cargo build --quiet
  S4_BIN="target/debug/s4"
fi
S4_BIN="$(cd "$(dirname "$S4_BIN")" && pwd)/$(basename "$S4_BIN")"

WORKDIR="$(mktemp -d)"
CFG_DIR="$WORKDIR/config"
# s4 gets its own TMPDIR so leftover temp files can be detected reliably.
S4_TMP="$WORKDIR/s4-tmp"
mkdir -p "$S4_TMP"

RUN_ID="$(date +%s)-$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
BUCKET="s4-reg-${RUN_ID}"
DST_BUCKET="s4-reg-dst-${RUN_ID}"
LOCK_BUCKET="s4-reg-lock-${RUN_ID}"
CROSS_BUCKET="s4-reg-x-${RUN_ID}"

s4() { TMPDIR="$S4_TMP" "$S4_BIN" -C "$CFG_DIR" "$@"; }
quiet() { "$@" >/dev/null 2>&1; }
fails() { ! "$@" >/dev/null 2>&1; }

cleanup() {
  for b in "$BUCKET" "$DST_BUCKET" "$LOCK_BUCKET"; do
    quiet s4 rb --force --bypass "t/$b" || true
  done
  if [[ -n "${S4_E2E_ENDPOINT2:-}" ]]; then
    quiet s4 rb --force "u/$CROSS_BUCKET" || true
  fi
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

PASSED=0
FAILED=()
check() {
  local name="$1"
  shift
  if "$@"; then
    echo "[regression] PASS $name"
    PASSED=$((PASSED + 1))
  else
    echo "[regression] FAIL $name" >&2
    FAILED+=("$name")
  fi
}

file_mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
lists_key() { s4 find "$1" | grep -qxF "$2"; }
count_keys() { s4 find "$1" | wc -l | tr -d ' '; }
json_is_valid() { python3 -c 'import json, sys; json.load(sys.stdin)'; }

s4 alias set t "$S4_E2E_ENDPOINT" "$S4_E2E_ACCESS_KEY" "$S4_E2E_SECRET_KEY" --region "$REGION" --path-style >/dev/null
s4 mb "t/$BUCKET" >/dev/null
s4 mb "t/$DST_BUCKET" >/dev/null
printf 'hello\n' > "$WORKDIR/hello.txt"

# sync --remove keeps destination objects skipped by --exclude
s4 put "$WORKDIR/hello.txt" "t/$BUCKET/src/keep.txt" >/dev/null
s4 put "$WORKDIR/hello.txt" "t/$BUCKET/src/app.log" >/dev/null
s4 put "$WORKDIR/hello.txt" "t/$DST_BUCKET/app.log" >/dev/null
s4 put "$WORKDIR/hello.txt" "t/$DST_BUCKET/stale.txt" >/dev/null
s4 sync --remove --exclude '*.log' "t/$BUCKET/src" "t/$DST_BUCKET" >/dev/null
check "sync --remove keeps excluded object" lists_key "t/$DST_BUCKET" app.log
check "sync --remove deletes extraneous object" fails lists_key "t/$DST_BUCKET" stale.txt

# sync prefixes are directory boundaries
s4 put "$WORKDIR/hello.txt" "t/$BUCKET/photos/a.txt" >/dev/null
s4 put "$WORKDIR/hello.txt" "t/$BUCKET/photos2/b.txt" >/dev/null
s4 sync "t/$BUCKET/photos" "t/$DST_BUCKET/p" >/dev/null
check "sync prefix excludes sibling prefixes" fails lists_key "t/$DST_BUCKET/p" p/photos2/b.txt
check "sync copies objects under the prefix" lists_key "t/$DST_BUCKET/p" p/a.txt

# repeated sync and --watch only act on changes
check "repeated sync copies nothing" grep -q "Synced 0 object" <(s4 sync "t/$BUCKET/photos" "t/$DST_BUCKET/p")
S4_SYNC_WATCH_INTERVAL_SEC=1 timeout 3.5 env TMPDIR="$S4_TMP" "$S4_BIN" -C "$CFG_DIR" \
  sync --watch "t/$BUCKET/photos" "t/$DST_BUCKET/p" > "$WORKDIR/watch.out" 2>&1
check "watch reports only passes with changes" test "$(wc -l < "$WORKDIR/watch.out")" -eq 1

# failed download keeps the destination file
echo IMPORTANT > "$WORKDIR/keep.txt"
s4 get "t/$BUCKET/missing.txt" "$WORKDIR/keep.txt" 2> "$WORKDIR/get.err"
check "failed get leaves destination intact" grep -qx IMPORTANT "$WORKDIR/keep.txt"
check "failed get reports the S3 error" grep -Eq "NoSuchKey|status 404" "$WORKDIR/get.err"
check "failed get leaves no partial file" fails ls "$WORKDIR"/.keep.txt.s4-partial-*

# rb requires --force for non-empty buckets
s4 rb "t/$DST_BUCKET" 2> "$WORKDIR/rb.err"
check "rb without --force refuses non-empty bucket" grep -q "use --force" "$WORKDIR/rb.err"
check "rb --force removes non-empty bucket" quiet s4 rb --force "t/$DST_BUCKET"

# governance retention is only bypassed on request
if s4 mb --with-lock "t/$LOCK_BUCKET" > "$WORKDIR/lock-mb.out" 2>&1; then
  s4 put "$WORKDIR/hello.txt" "t/$LOCK_BUCKET/obj.txt" >/dev/null
  s4 retention set "t/$LOCK_BUCKET/obj.txt" --mode GOVERNANCE --retain-until 2030-01-01T00:00:00Z >/dev/null
  if quiet s4 rm "t/$LOCK_BUCKET/obj.txt"; then
    echo "[regression] note: rm without --bypass created a delete marker (versioned bucket semantics)"
  fi
  check "rb --force without --bypass keeps governance-locked versions" fails s4 rb --force "t/$LOCK_BUCKET"
  s4 put "$WORKDIR/hello.txt" "t/$LOCK_BUCKET/obj2.txt" >/dev/null
  s4 retention set "t/$LOCK_BUCKET/obj2.txt" --mode GOVERNANCE --retain-until 2030-01-01T00:00:00Z >/dev/null
  check "retention clear succeeds" quiet s4 retention clear "t/$LOCK_BUCKET/obj2.txt"
  check "retention clear removes the mode" fails grep -q GOVERNANCE <(s4 retention info "t/$LOCK_BUCKET/obj2.txt" 2>&1)
  check "rb --force --bypass removes locked bucket" quiet s4 rb --force --bypass "t/$LOCK_BUCKET"
else
  echo "[regression] skipping object-lock checks: $(head -c 200 "$WORKDIR/lock-mb.out")" >&2
fi

# config holds secrets and must be private
check "config file is private (0600)" test "$(file_mode "$CFG_DIR/config.toml")" = 600

# listings beyond 1000 keys (continuation token is signed correctly)
seq 1 1005 | xargs -P 8 -I{} env TMPDIR="$S4_TMP" "$S4_BIN" -C "$CFG_DIR" \
  put "$WORKDIR/hello.txt" "t/$BUCKET/many/k{}" >/dev/null
check "find lists more than 1000 keys" test "$(count_keys "t/$BUCKET/many")" -eq 1005

# prefixes and keys with reserved characters are signed correctly
s4 put "$WORKDIR/hello.txt" "t/$BUCKET/deep/er/x y+z&.txt" >/dev/null
check "nested prefix with '/' lists keys" lists_key "t/$BUCKET/deep/er/" "deep/er/x y+z&.txt"
check "stat of key with reserved characters" quiet s4 stat "t/$BUCKET/deep/er/x y+z&.txt"

# output format
check "--json stat is valid JSON" json_is_valid < <(s4 --json stat "t/$BUCKET/photos/a.txt")
check "uploads get a Content-Type from the extension" grep -qi '^content-type: text/plain' <(s4 stat "t/$BUCKET/photos/a.txt")

# streaming reads
head -c 300000 /dev/urandom > "$WORKDIR/bin.dat"
s4 put "$WORKDIR/bin.dat" "t/$BUCKET/bin.dat" >/dev/null
s4 cat "t/$BUCKET/bin.dat" > "$WORKDIR/bin.out"
check "cat is binary safe" cmp -s "$WORKDIR/bin.dat" "$WORKDIR/bin.out"
check "cat of a missing key fails" fails s4 cat "t/$BUCKET/nope"
yes line | head -200000 > "$WORKDIR/lines.txt"
s4 put "$WORKDIR/lines.txt" "t/$BUCKET/lines.txt" >/dev/null
s4 head "t/$BUCKET/lines.txt" 100000 2> "$WORKDIR/head.err" | head -1 >/dev/null
check "head into a closed pipe does not panic" fails grep -q panicked "$WORKDIR/head.err"
check "head prints N lines" test "$(s4 head "t/$BUCKET/lines.txt" 3 | wc -l | tr -d ' ')" -eq 3

# pipe streams large stdin through multipart
head -c 20000000 /dev/urandom > "$WORKDIR/big.dat"
s4 pipe "t/$BUCKET/piped.dat" < "$WORKDIR/big.dat" >/dev/null
s4 get "t/$BUCKET/piped.dat" "$WORKDIR/piped.out" >/dev/null
check "pipe over 16 MiB round-trips" cmp -s "$WORKDIR/big.dat" "$WORKDIR/piped.out"

# x-amz-* headers are signed
check "server-side copy (signed x-amz-copy-source)" quiet s4 cp "t/$BUCKET/photos/a.txt" "t/$BUCKET/copied.txt"
check "custom x-amz header is signed" quiet s4 -H "x-amz-meta-regression: 1" put "$WORKDIR/hello.txt" "t/$BUCKET/meta.txt"

# cp/sync between different endpoints
if [[ -n "${S4_E2E_ENDPOINT2:-}" ]]; then
  s4 alias set u "$S4_E2E_ENDPOINT2" "${S4_E2E_ACCESS_KEY2:?}" "${S4_E2E_SECRET_KEY2:?}" --region "$REGION" --path-style >/dev/null
  s4 mb "u/$CROSS_BUCKET" >/dev/null
  s4 cp "t/$BUCKET/bin.dat" "u/$CROSS_BUCKET/bin.dat" >/dev/null
  s4 get "u/$CROSS_BUCKET/bin.dat" "$WORKDIR/cross.out" >/dev/null
  check "cp between endpoints" cmp -s "$WORKDIR/bin.dat" "$WORKDIR/cross.out"
  s4 sync "t/$BUCKET/photos" "u/$CROSS_BUCKET/p" >/dev/null
  check "sync between endpoints" quiet s4 stat "u/$CROSS_BUCKET/p/a.txt"
else
  echo "[regression] skipping cross-endpoint checks: S4_E2E_ENDPOINT2 is not set"
fi

# explicit errors instead of silently ignored input
check "sql --enc-c is rejected" fails s4 sql --enc-c "t/$BUCKET=a2V5" "t/$BUCKET/x.csv"
check "retention set rejects an unknown mode" fails s4 retention set "t/$BUCKET/x" --mode NOPE --retain-until 2030-01-01T00:00:00Z

check "no temp files left behind" test -z "$(ls -A "$S4_TMP")"

echo "[regression] passed: $PASSED, failed: ${#FAILED[@]}"
if (( ${#FAILED[@]} > 0 )); then
  printf '[regression] failed: %s\n' "${FAILED[@]}" >&2
  exit 1
fi
