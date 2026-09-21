#!/usr/bin/env bash
set -euo pipefail

: "${S4_E2E_ENDPOINT:?S4_E2E_ENDPOINT is required}"
: "${S4_E2E_ACCESS_KEY:?S4_E2E_ACCESS_KEY is required}"
: "${S4_E2E_SECRET_KEY:?S4_E2E_SECRET_KEY is required}"

S4_E2E_REGION="${S4_E2E_REGION:-us-east-1}"
S4_E2E_REMOTE_LIMITED="${S4_E2E_REMOTE_LIMITED:-0}"

# First run full CRUD flow.
S4_E2E_ALIAS="ci-e2e"
S4_E2E_PREFIX="s4-ci-e2e"
S4_E2E_PATH_STYLE="1"
./scripts/e2e.sh

# Then run dedicated sync flow.
WORKDIR="$(mktemp -d)"
CFG_DIR="$WORKDIR/config"
trap 'rm -rf "$WORKDIR"' EXIT

TS="$(date +%s)"
RUN_ID="${TS}-$(openssl rand -hex 3 2>/dev/null || date +%N | tail -c 7)"
SRC_BUCKET="s4-sync-src-${RUN_ID}"
DST_BUCKET="s4-sync-dst-${RUN_ID}"
SRC1="$WORKDIR/src1.txt"
SRC2="$WORKDIR/src2.txt"
OUT1="$WORKDIR/out1.txt"
OUT2="$WORKDIR/out2.txt"
SKIPPED_CAPABILITIES=()
NOT_IMPLEMENTED_ON_SERVER=()

printf 'sync-one-%s\n' "$TS" > "$SRC1"
printf 'sync-two-%s\n' "$TS" > "$SRC2"

cargo build

target/debug/s4 -C "$CFG_DIR" alias set ci "$S4_E2E_ENDPOINT" "$S4_E2E_ACCESS_KEY" "$S4_E2E_SECRET_KEY" --region "$S4_E2E_REGION" --path-style

target/debug/s4 -C "$CFG_DIR" mb "ci/$SRC_BUCKET"
target/debug/s4 -C "$CFG_DIR" mb "ci/$DST_BUCKET"



# Pattern matcher with fallback when ripgrep is unavailable on runner image.
has_pattern() {
  local pattern="$1"
  local file="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q "$pattern" "$file"
  else
    grep -Eq "$pattern" "$file"
  fi
}

# Some bucket-level management APIs may be unavailable in certain MinIO builds/configurations.
# In that case we skip those checks on explicit 501 NotImplemented responses.
run_or_skip_not_implemented() {
  local out_file="$1"
  shift
  if "$@" >"$out_file" 2>&1; then
    cat "$out_file"
    return 0
  fi
  if has_pattern "status 501|<Code>NotImplemented</Code>|not implemented|<Code>InvalidArgument</Code>.*destination ARN|destination ARN does not exist or is not well-formed" "$out_file"; then
    echo "[ci] skipping unsupported API call: $*" >&2
    return 10
  fi
  cat "$out_file" >&2
  return 1
}

is_object_lock_unsupported_error() {
  local file="$1"
  has_pattern "Object Lock not enabled on bucket|ObjectLockConfigurationNotFoundError|InvalidRequest.*Object Lock|Object Lock configuration does not exist|XNotImplemented|NotImplemented" "$file"
}

is_sql_unsupported_error() {
  local file="$1"
  has_pattern "status 501|<Code>NotImplemented</Code>|<Code>InvalidRequest</Code>.*Select|Select.*not supported|S3 Select is not enabled|Unsupported\s*operation|Unsupported.*Select|<Code>XNotImplemented</Code>|<Code>MethodNotAllowed</Code>" "$file"
}

is_sql_generic_400() {
  local file="$1"
  has_pattern "request failed with status 400(:|$)" "$file"
}

mark_capability_skipped() {
  local cap="$1"
  for existing in "${SKIPPED_CAPABILITIES[@]}"; do
    if [[ "$existing" == "$cap" ]]; then
      return 0
    fi
  done
  SKIPPED_CAPABILITIES+=("$cap")
}

mark_not_implemented_on_server() {
  local cap="$1"
  for existing in "${NOT_IMPLEMENTED_ON_SERVER[@]}"; do
    if [[ "$existing" == "$cap" ]]; then
      return 0
    fi
  done
  NOT_IMPLEMENTED_ON_SERVER+=("$cap")
  mark_capability_skipped "$cap"
}

skip_if_remote_limited() {
  local cap="$1"
  if [[ "$S4_E2E_REMOTE_LIMITED" == "1" ]]; then
    echo "[ci] skipping ${cap}: not implemented on remote server profile" >&2
    mark_not_implemented_on_server "$cap"
    return 0
  fi
  return 1
}

purge_bucket_objects_best_effort() {
  local alias_name="$1"
  local bucket_name="$2"
  local xml_out="$WORKDIR/purge-${bucket_name}.xml"
  local keys_out="$WORKDIR/purge-${bucket_name}.keys"

  if ! target/debug/s4 -C "$CFG_DIR" ls "${alias_name}/${bucket_name}" > "$xml_out" 2>/dev/null; then
    return 0
  fi

  python3 - "$xml_out" > "$keys_out" <<'PYKEYS'
import html
import re
import sys
text = open(sys.argv[1], "r", encoding="utf-8", errors="ignore").read()
for key in re.findall(r"<Key>(.*?)</Key>", text, flags=re.S):
    print(html.unescape(key.strip()))
PYKEYS

  while IFS= read -r key; do
    [[ -z "$key" ]] && continue
    target/debug/s4 -C "$CFG_DIR" rm "${alias_name}/${bucket_name}/${key}" >/dev/null 2>&1 || true
  done < "$keys_out"
}

# cors coverage
CORS_XML="$WORKDIR/cors.xml"
cat > "$CORS_XML" <<'EOF'
<CORSConfiguration>
  <CORSRule>
    <AllowedOrigin>*</AllowedOrigin>
    <AllowedMethod>GET</AllowedMethod>
    <AllowedMethod>PUT</AllowedMethod>
    <AllowedHeader>*</AllowedHeader>
  </CORSRule>
</CORSConfiguration>
EOF

if run_or_skip_not_implemented "$WORKDIR/cors-set.out" target/debug/s4 -C "$CFG_DIR" cors set "ci/$SRC_BUCKET" "$CORS_XML"; then
  if run_or_skip_not_implemented "$WORKDIR/cors-get.out" target/debug/s4 -C "$CFG_DIR" cors get "ci/$SRC_BUCKET"; then
    has_pattern "CORSConfiguration|CORSRule" "$WORKDIR/cors-get.out"
    run_or_skip_not_implemented "$WORKDIR/cors-remove.out" target/debug/s4 -C "$CFG_DIR" cors remove "ci/$SRC_BUCKET" || true
  fi
fi

# encrypt coverage
if skip_if_remote_limited "encryption"; then
  :
else
ENC_XML="$WORKDIR/encryption.xml"
cat > "$ENC_XML" <<'EOF'
<ServerSideEncryptionConfiguration>
  <Rule>
    <ApplyServerSideEncryptionByDefault>
      <SSEAlgorithm>AES256</SSEAlgorithm>
    </ApplyServerSideEncryptionByDefault>
  </Rule>
</ServerSideEncryptionConfiguration>
EOF

if run_or_skip_not_implemented "$WORKDIR/encrypt-set.out" target/debug/s4 -C "$CFG_DIR" encrypt set "ci/$SRC_BUCKET" "$ENC_XML"; then
  if run_or_skip_not_implemented "$WORKDIR/encrypt-info.out" target/debug/s4 -C "$CFG_DIR" encrypt info "ci/$SRC_BUCKET"; then
    has_pattern "ServerSideEncryptionConfiguration|SSEAlgorithm" "$WORKDIR/encrypt-info.out"
    run_or_skip_not_implemented "$WORKDIR/encrypt-clear.out" target/debug/s4 -C "$CFG_DIR" encrypt clear "ci/$SRC_BUCKET" || true
  fi
fi
fi

# event coverage
if skip_if_remote_limited "event-notifications"; then
  :
else
EVENT_XML="$WORKDIR/notification.xml"
cat > "$EVENT_XML" <<'EOF'
<NotificationConfiguration>
  <QueueConfiguration>
    <Id>s4-ci-event</Id>
    <Event>s3:ObjectCreated:*</Event>
    <Queue>arn:minio:sqs::1:webhook</Queue>
  </QueueConfiguration>
</NotificationConfiguration>
EOF

if run_or_skip_not_implemented "$WORKDIR/event-add.out" target/debug/s4 -C "$CFG_DIR" event add "ci/$SRC_BUCKET" "$EVENT_XML"; then
  if run_or_skip_not_implemented "$WORKDIR/event-list.out" target/debug/s4 -C "$CFG_DIR" event ls "ci/$SRC_BUCKET"; then
    has_pattern "NotificationConfiguration|QueueConfiguration|Event" "$WORKDIR/event-list.out"
    run_or_skip_not_implemented "$WORKDIR/event-rm.out" target/debug/s4 -C "$CFG_DIR" event rm "ci/$SRC_BUCKET" --force || true
  fi
fi
fi


expect_placeholder_or_unknown() {
  local out_file="$1"
  shift
  if "$@" >"$out_file" 2>&1; then
    has_pattern "not implemented|unknown command" "$out_file"
    return 0
  fi
  has_pattern "not implemented|unknown command" "$out_file"
}

# idp coverage (placeholder behavior)
if skip_if_remote_limited "idp-openid"; then :; else expect_placeholder_or_unknown "$WORKDIR/idp-openid.out" target/debug/s4 -C "$CFG_DIR" idp openid; fi
if skip_if_remote_limited "idp-ldap"; then :; else expect_placeholder_or_unknown "$WORKDIR/idp-ldap.out" target/debug/s4 -C "$CFG_DIR" idp ldap; fi

# ilm coverage (placeholder behavior)
if skip_if_remote_limited "ilm-advanced"; then :; else expect_placeholder_or_unknown "$WORKDIR/ilm-rule.out" target/debug/s4 -C "$CFG_DIR" ilm rule; expect_placeholder_or_unknown "$WORKDIR/ilm-restore.out" target/debug/s4 -C "$CFG_DIR" ilm restore; fi

# legalhold/retention coverage (requires bucket with object lock)
LH_BUCKET="s4-legalhold-${RUN_ID}"
LH_LOCAL="$WORKDIR/legalhold.txt"
LH_GOT="$WORKDIR/legalhold-got.txt"
printf 'legalhold-%s
' "$TS" > "$LH_LOCAL"
target/debug/s4 -C "$CFG_DIR" mb --with-lock "ci/$LH_BUCKET"
target/debug/s4 -C "$CFG_DIR" put "$LH_LOCAL" "ci/$LH_BUCKET/lh.txt"
if target/debug/s4 -C "$CFG_DIR" legalhold set "ci/$LH_BUCKET/lh.txt" > "$WORKDIR/legalhold-set.out" 2>&1; then
  target/debug/s4 -C "$CFG_DIR" legalhold info "ci/$LH_BUCKET/lh.txt" > "$WORKDIR/legalhold-info-on.out"
  has_pattern "<Status>ON</Status>|ON" "$WORKDIR/legalhold-info-on.out"
  target/debug/s4 -C "$CFG_DIR" legalhold clear "ci/$LH_BUCKET/lh.txt"
  target/debug/s4 -C "$CFG_DIR" legalhold info "ci/$LH_BUCKET/lh.txt" > "$WORKDIR/legalhold-info-off.out"
  has_pattern "<Status>OFF</Status>|OFF" "$WORKDIR/legalhold-info-off.out"

  # retention coverage (requires object-lock bucket)
  RET_UNTIL="2030-01-01T00:00:00Z"
  target/debug/s4 -C "$CFG_DIR" retention set "ci/$LH_BUCKET/lh.txt" --mode GOVERNANCE --retain-until "$RET_UNTIL"
  target/debug/s4 -C "$CFG_DIR" retention info "ci/$LH_BUCKET/lh.txt" > "$WORKDIR/retention-info.out"
  has_pattern "GOVERNANCE|Mode|RetainUntilDate" "$WORKDIR/retention-info.out"
  target/debug/s4 -C "$CFG_DIR" retention clear "ci/$LH_BUCKET/lh.txt"
  target/debug/s4 -C "$CFG_DIR" get "ci/$LH_BUCKET/lh.txt" "$LH_GOT"
  cmp -s "$LH_LOCAL" "$LH_GOT"
  # Some object-lock servers deny DELETE without versionId even when governance bypass is used.
  # Best-effort object delete first, then rely on `rb` version-purge path for authoritative cleanup.
  target/debug/s4 -C "$CFG_DIR" rm "ci/$LH_BUCKET/lh.txt" > "$WORKDIR/legalhold-rm.out" 2>&1 || true
  if target/debug/s4 -C "$CFG_DIR" rb "ci/$LH_BUCKET" > "$WORKDIR/legalhold-rb.out" 2>&1; then
    cat "$WORKDIR/legalhold-rb.out"
  else
    cat "$WORKDIR/legalhold-rb.out" >&2
    if [[ "$S4_E2E_REMOTE_LIMITED" == "1" ]] && has_pattern "AccessDenied|status 403|versionId is required|Object Lock" "$WORKDIR/legalhold-rb.out"; then
      echo "[ci] skipping legalhold/retention cleanup strictness: remote server denied object-lock delete path" >&2
      mark_not_implemented_on_server "object-lock"
    else
      exit 1
    fi
  fi
else
  cat "$WORKDIR/legalhold-set.out" >&2
  if is_object_lock_unsupported_error "$WORKDIR/legalhold-set.out"; then
    echo "[ci] skipping legalhold/retention checks: object lock is not enabled/supported on remote bucket" >&2
    mark_capability_skipped "object-lock"
    target/debug/s4 -C "$CFG_DIR" rm "ci/$LH_BUCKET/lh.txt" || true
    target/debug/s4 -C "$CFG_DIR" rb "ci/$LH_BUCKET" || true
  else
    exit 1
  fi
fi

# replicate coverage
if skip_if_remote_limited "replication"; then
  :
else
if target/debug/s4 -C "$CFG_DIR" replicate ls "ci/$SRC_BUCKET" > "$WORKDIR/replicate-ls.out"; then
  has_pattern "not implemented" "$WORKDIR/replicate-ls.out"
else
  echo "[ci] replicate ls command unexpectedly failed" >&2
  exit 1
fi
if target/debug/s4 -C "$CFG_DIR" replicate backlog "ci/$SRC_BUCKET" > "$WORKDIR/replicate-backlog.out"; then
  has_pattern "not implemented" "$WORKDIR/replicate-backlog.out"
else
  echo "[ci] replicate backlog command unexpectedly failed" >&2
  exit 1
fi
fi

# unsupported mc command compatibility checks (must fail explicitly until implemented)
expect_unknown_command() {
  local cmd_name="$1"
  local out_file="$WORKDIR/unsupported-${cmd_name}.out"
  if target/debug/s4 -C "$CFG_DIR" "$cmd_name" >"$out_file" 2>&1; then
    echo "[ci] expected unsupported command '$cmd_name' to fail" >&2
    exit 1
  fi
  has_pattern "unknown command|not implemented|usage:" "$out_file"
}

if skip_if_remote_limited "admin"; then :; else expect_unknown_command admin; fi
if skip_if_remote_limited "anonymous-extras"; then :; else expect_unknown_command anonymous; fi
if skip_if_remote_limited "batch-jobs"; then :; else expect_unknown_command batch; fi
expect_unknown_command diff
expect_unknown_command du
expect_unknown_command od
if skip_if_remote_limited "bucket-quota"; then :; else expect_unknown_command quota; fi
if skip_if_remote_limited "support"; then :; else expect_unknown_command support; fi
expect_unknown_command share
expect_unknown_command tag
expect_unknown_command undo
expect_unknown_command update
expect_unknown_command watch
if skip_if_remote_limited "license"; then :; else expect_unknown_command license; fi

# version command coverage
S4_VERSION_OUT="$WORKDIR/version.out"
target/debug/s4 -C "$CFG_DIR" version > "$S4_VERSION_OUT"
has_pattern "s4 " "$S4_VERSION_OUT"

# global flags coverage: resolve/custom header/limits
EP_HOSTPORT="${S4_E2E_ENDPOINT#http://}"
EP_HOSTPORT="${EP_HOSTPORT#https://}"
if [[ "$EP_HOSTPORT" == *":"* ]]; then
  EP_HOST="${EP_HOSTPORT%%:*}"
  EP_PORT="${EP_HOSTPORT##*:}"
else
  EP_HOST="$EP_HOSTPORT"
  EP_PORT="80"
fi

target/debug/s4 -C "$CFG_DIR"   --resolve "${EP_HOST}:${EP_PORT}=${EP_HOST}"   --limit-download "1G"   --custom-header "x-s4-ci: globals"   ls ci > "$WORKDIR/globals-ls.out"

# ping/ready coverage
target/debug/s4 -C "$CFG_DIR" ping ci > "$WORKDIR/ping.out"
has_pattern "alive|latency_ms" "$WORKDIR/ping.out"

if skip_if_remote_limited "ready"; then
  :
else
  target/debug/s4 -C "$CFG_DIR" ready ci > "$WORKDIR/ready.out"
  has_pattern "ready" "$WORKDIR/ready.out"
fi

target/debug/s4 -C "$CFG_DIR" put "$SRC1" "ci/$SRC_BUCKET/photos/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" put "$SRC2" "ci/$SRC_BUCKET/photos/2024/b.txt"

# sql coverage (S3 Select API)
if skip_if_remote_limited "s3-select"; then
  :
else
SQL_CSV="$WORKDIR/sql-data.csv"
printf 'id,name
1,alice
2,bob
' > "$SQL_CSV"
target/debug/s4 -C "$CFG_DIR" put "$SQL_CSV" "ci/$SRC_BUCKET/sql/data.csv"
if target/debug/s4 -C "$CFG_DIR" sql --csv-input "fh=USE" --query "select count(*) from S3Object s" "ci/$SRC_BUCKET/sql/data.csv" > "$WORKDIR/sql-single.out" 2>&1; then
  has_pattern "2" "$WORKDIR/sql-single.out"
  target/debug/s4 -C "$CFG_DIR" sql --recursive --csv-input "fh=USE" --query "select s.name from S3Object s" "ci/$SRC_BUCKET/sql" > "$WORKDIR/sql-recursive.out"
  has_pattern "alice|bob" "$WORKDIR/sql-recursive.out"
else
  cat "$WORKDIR/sql-single.out" >&2
  if is_sql_unsupported_error "$WORKDIR/sql-single.out"; then
    echo "[ci] skipping sql checks: S3 Select is not enabled/supported on remote endpoint" >&2
    mark_capability_skipped "s3-select"
  elif is_sql_generic_400 "$WORKDIR/sql-single.out"; then
    # Some S3-compatible services return a bare HTTP 400 for unsupported Select requests.
    # Probe one more query to avoid hiding transient or query-specific failures.
    if target/debug/s4 -C "$CFG_DIR" sql --csv-input "fh=USE" --query "select * from S3Object s limit 1" "ci/$SRC_BUCKET/sql/data.csv" > "$WORKDIR/sql-probe.out" 2>&1; then
      cat "$WORKDIR/sql-probe.out" >&2
      exit 1
    fi
    cat "$WORKDIR/sql-probe.out" >&2
    if is_sql_unsupported_error "$WORKDIR/sql-probe.out" || is_sql_generic_400 "$WORKDIR/sql-probe.out"; then
      echo "[ci] skipping sql checks: remote endpoint rejects S3 Select requests (generic 400)" >&2
      mark_capability_skipped "s3-select"
    else
      exit 1
    fi
  else
    exit 1
  fi
fi
fi

target/debug/s4 -C "$CFG_DIR" sync "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/sync-copy"
target/debug/s4 -C "$CFG_DIR" mirror "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/mirror-copy"

target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/sync-copy/2024/a.txt" "$OUT1"
target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/mirror-copy/2024/b.txt" "$OUT2"

cmp -s "$SRC1" "$OUT1"
cmp -s "$SRC2" "$OUT2"

# mirror/sync flags coverage: --dry-run should not copy
DRYRUN_OUT="$WORKDIR/dryrun.out"
target/debug/s4 -C "$CFG_DIR" mirror --dry-run "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/dry-run" > "$DRYRUN_OUT"
has_pattern "dry-run: true" "$DRYRUN_OUT"
if target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/dry-run/2024/a.txt" "$WORKDIR/dryrun-got.txt" >/dev/null 2>&1; then
  echo "[ci] dry-run unexpectedly copied object" >&2
  exit 1
fi

# --exclude should skip matching keys
EXCL_LOCAL="$WORKDIR/exclude.tmp"
printf 'exclude-me-%s
' "$TS" > "$EXCL_LOCAL"
target/debug/s4 -C "$CFG_DIR" put "$EXCL_LOCAL" "ci/$SRC_BUCKET/photos/2024/exclude.tmp"
target/debug/s4 -C "$CFG_DIR" sync --exclude "*.tmp" "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/exclude-copy"
if target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/exclude-copy/2024/exclude.tmp" "$WORKDIR/exclude-got.tmp" >/dev/null 2>&1; then
  echo "[ci] --exclude did not filter *.tmp" >&2
  exit 1
fi

# --remove should delete extraneous object on destination
target/debug/s4 -C "$CFG_DIR" cp "$SRC1" "ci/$DST_BUCKET/sync-copy/2024/extraneous.txt"
target/debug/s4 -C "$CFG_DIR" sync --remove "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/sync-copy"
if target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/sync-copy/2024/extraneous.txt" "$WORKDIR/extraneous.txt" >/dev/null 2>&1; then
  echo "[ci] --remove did not clean extraneous object" >&2
  exit 1
fi


# --older-than should skip fresh objects
target/debug/s4 -C "$CFG_DIR" sync --older-than "365d" "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/older-than"
if target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/older-than/2024/a.txt" "$WORKDIR/older-than-a.txt" >/dev/null 2>&1; then
  echo "[ci] --older-than unexpectedly copied fresh object" >&2
  exit 1
fi

# --newer-than should include fresh objects
target/debug/s4 -C "$CFG_DIR" sync --newer-than "365d" "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/newer-than"
target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/newer-than/2024/a.txt" "$WORKDIR/newer-than-a.txt"
cmp -s "$SRC1" "$WORKDIR/newer-than-a.txt"


# --watch should continuously mirror new objects
WATCH_EXPECT="$WORKDIR/watch.txt"
WATCH_GOT="$WORKDIR/watch-got.txt"
WATCH_LOG="$WORKDIR/watch.log"
printf 'watch-check-%s
' "$TS" > "$WATCH_EXPECT"
S4_SYNC_WATCH_INTERVAL_SEC=1 target/debug/s4 -C "$CFG_DIR" sync --watch "ci/$SRC_BUCKET/photos" "ci/$DST_BUCKET/watch-copy" > "$WATCH_LOG" 2>&1 &
WATCH_PID=$!
cleanup_watch() { kill "$WATCH_PID" 2>/dev/null || true; wait "$WATCH_PID" 2>/dev/null || true; }
trap 'cleanup_watch; rm -rf "$WORKDIR"' EXIT
sleep 2
target/debug/s4 -C "$CFG_DIR" put "$WATCH_EXPECT" "ci/$SRC_BUCKET/photos/2024/watch.txt"
for _ in $(seq 1 15); do
  if target/debug/s4 -C "$CFG_DIR" get "ci/$DST_BUCKET/watch-copy/2024/watch.txt" "$WATCH_GOT" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
cmp -s "$WATCH_EXPECT" "$WATCH_GOT"
cleanup_watch
trap 'rm -rf "$WORKDIR"' EXIT

# find/tree/head coverage
target/debug/s4 -C "$CFG_DIR" find "ci/$SRC_BUCKET/photos" "2024" > "$WORKDIR/find.out"
has_pattern "photos/2024/a.txt|2024/a.txt" "$WORKDIR/find.out"

target/debug/s4 -C "$CFG_DIR" tree "ci/$SRC_BUCKET/photos" > "$WORKDIR/tree.out"
has_pattern "a.txt" "$WORKDIR/tree.out"

target/debug/s4 -C "$CFG_DIR" head "ci/$SRC_BUCKET/photos/2024/a.txt" 1 > "$WORKDIR/head.out"
has_pattern "sync-one" "$WORKDIR/head.out"

# cp/mv coverage
CP_LOCAL="$WORKDIR/cp-local.txt"
CP_BACK="$WORKDIR/cp-back.txt"
printf 'cp-check-%s
' "$TS" > "$CP_LOCAL"

target/debug/s4 -C "$CFG_DIR" cp "$CP_LOCAL" "ci/$SRC_BUCKET/cp/local.txt"
target/debug/s4 -C "$CFG_DIR" cp "ci/$SRC_BUCKET/cp/local.txt" "$CP_BACK"
cmp -s "$CP_LOCAL" "$CP_BACK"

target/debug/s4 -C "$CFG_DIR" cp "ci/$SRC_BUCKET/cp/local.txt" "ci/$DST_BUCKET/cp/copied.txt"
target/debug/s4 -C "$CFG_DIR" mv "ci/$DST_BUCKET/cp/copied.txt" "ci/$DST_BUCKET/cp/moved.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/cp/local.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/cp/moved.txt"


# pipe coverage
PIPE_EXPECT="$WORKDIR/pipe-expected.txt"
PIPE_GOT="$WORKDIR/pipe-got.txt"
printf 'pipe-check-%s
' "$TS" > "$PIPE_EXPECT"
cat "$PIPE_EXPECT" | target/debug/s4 -C "$CFG_DIR" pipe "ci/$SRC_BUCKET/pipe/stdin.txt"
target/debug/s4 -C "$CFG_DIR" get "ci/$SRC_BUCKET/pipe/stdin.txt" "$PIPE_GOT"
cmp -s "$PIPE_EXPECT" "$PIPE_GOT"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/pipe/stdin.txt"


# multipart upload coverage (file > threshold)
MP_LOCAL="$WORKDIR/multipart.bin"
MP_GOT="$WORKDIR/multipart-got.bin"
python3 - <<'PYS' > "$MP_LOCAL"
import sys
sys.stdout.buffer.write(b'Z' * (17 * 1024 * 1024))
PYS

target/debug/s4 -C "$CFG_DIR" put "$MP_LOCAL" "ci/$SRC_BUCKET/mp/large.bin"
target/debug/s4 -C "$CFG_DIR" get "ci/$SRC_BUCKET/mp/large.bin" "$MP_GOT"
cmp -s "$MP_LOCAL" "$MP_GOT"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/mp/large.bin"

target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/photos/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/photos/2024/b.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/photos/2024/exclude.tmp"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/sync-copy/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/sync-copy/2024/b.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/sync-copy/2024/exclude.tmp"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/mirror-copy/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/mirror-copy/2024/b.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/exclude-copy/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/exclude-copy/2024/b.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/newer-than/2024/a.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/newer-than/2024/b.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/newer-than/2024/exclude.tmp"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/photos/2024/watch.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$DST_BUCKET/watch-copy/2024/watch.txt"
target/debug/s4 -C "$CFG_DIR" rm "ci/$SRC_BUCKET/sql/data.csv"

purge_bucket_objects_best_effort "ci" "$SRC_BUCKET"
purge_bucket_objects_best_effort "ci" "$DST_BUCKET"
target/debug/s4 -C "$CFG_DIR" rb "ci/$SRC_BUCKET"
target/debug/s4 -C "$CFG_DIR" rb "ci/$DST_BUCKET"
target/debug/s4 -C "$CFG_DIR" alias rm ci

if (( ${#NOT_IMPLEMENTED_ON_SERVER[@]} > 0 )); then
  echo "[ci] ⚠️ NOT IMPLEMENTED ON THE SERVER ⚠️: $(IFS=', '; echo "${NOT_IMPLEMENTED_ON_SERVER[*]}")"
fi
if (( ${#SKIPPED_CAPABILITIES[@]} > 0 )); then
  echo "[ci] ⚠️ SKIPPED CAPABILITIES ⚠️: $(IFS=', '; echo "${SKIPPED_CAPABILITIES[*]}")"
fi

echo "[ci] S3 integration cases passed"
