# s4

`s4` — a CLI tool for S3/MinIO written in Rust, in the style of `mc`.

## What is implemented

- Global flags: `-C/--config-dir`, `--json`, `--debug`, `--insecure`, `--resolve`, `--limit-upload`, `--limit-download`, `--custom-header/-H`.
- Alias management: `alias set|ls|rm`.
- S3 commands: `ls`, `mb`, `rb`, `put`, `get`, `rm`, `stat`, `cat`, `cors`, `encrypt`, `event`, `legalhold`, `retention`, `sql`, `idp`, `ilm`, `replicate`, `sync`, `mirror` (alias for `sync`), `cp`, `mv`, `find`, `tree`, `head`, `pipe`, `ping`, `ready`.
- AWS SigV4: the canonical request is built in Rust (sorted, encoded query; every `x-amz-*` header signed); the HMAC chain and file hashing run in `python3`, HTTP calls go through `curl`. The secret key and the request headers (including `Authorization`) are passed to `python3`/`curl` on stdin, so they do not appear in the process list. The exception is `alias set`, which takes the keys as command-line arguments (they are visible in the process list and shell history while it runs).
- Uploads of 16 MiB and more use multipart upload (`put`, `pipe`, `cp` from a local file, and `cp`/`sync`/`mirror` between different accounts; within one account objects are copied server-side); the part size grows with the file so uploads fit into the 10000-part limit. `pipe` keeps at most 16 MiB of stdin in memory and spools the parts through temporary files.
- Uploaded objects get a `Content-Type` guessed from the key's extension (`application/octet-stream` otherwise).
- `get`/`cp` to a local file write to a hidden temporary file next to the destination and replace it only on success, so a failed download never clobbers an existing file. `cat` and `head` stream the object (binary safe; `head` stops downloading after N lines).
- Config format: `~/.s4/config.toml` (tab-separated; created with mode `0600` because it stores secret keys).
- Temporary files live in a private per-process directory (mode `0700`) under `$TMPDIR` and are removed on exit.

> The current build only supports path-style addressing: create aliases with `--path-style`, otherwise every request fails with `only --path-style aliases are supported in this build`.

## Requirements and build

- Runtime: `curl` 7.55+ and `python3` in `PATH`.
- Build: a current stable Rust toolchain (edition 2024).

```bash
cargo build --release     # binary: target/release/s4
cargo install --path .    # installs `s4` into ~/.cargo/bin
```

## Quick start

```bash
s4 alias set local http://127.0.0.1:9000 minio minio123 --path-style
s4 mb local/test-bucket
echo hello > hello.txt
s4 put hello.txt local/test-bucket/hello.txt
s4 cat local/test-bucket/hello.txt
s4 get local/test-bucket/hello.txt ./downloaded.txt
s4 stat local/test-bucket/hello.txt

# copy / move (local <-> S3 and S3 <-> S3)
s4 cp ./hello.txt local/test-bucket/docs/local.txt
s4 cp local/test-bucket/docs/local.txt ./local-copy.txt
s4 mv local/test-bucket/docs/local.txt local/test-bucket/docs/moved.txt

# find / tree / head
s4 find local/test-bucket docs
s4 tree local/test-bucket
s4 head local/test-bucket/docs/moved.txt 5

# upload from stdin
echo "stream data" | s4 pipe local/test-bucket/stdin.txt

# synchronization (equivalent of mc mirror)
s4 mb local/backup-bucket
s4 sync local/test-bucket local/backup-bucket
# or in mc style; --remove also deletes objects that no longer exist in the source
s4 mirror --remove local/test-bucket local/backup-bucket

# sql select (S3 Select API)
printf 'id,name\n1,alice\n2,bob\n' > data.csv
s4 put data.csv local/test-bucket/reports/data.csv
s4 sql --csv-input fh=USE --query "select s.name from S3Object s" local/test-bucket/reports/data.csv
s4 sql -r --csv-input fh=USE --query "select count(*) from S3Object" local/test-bucket/reports/

# checks
s4 ping local
s4 ready local

# cleanup: rb refuses non-empty buckets unless --force is given
s4 rm local/test-bucket/hello.txt
s4 rb --force local/test-bucket
s4 rb --force local/backup-bucket
```

Bucket configuration, object lock and placeholder commands:

```bash
s4 mb local/config-bucket

# cors
s4 cors set local/config-bucket ./cors.xml
s4 cors get local/config-bucket
s4 cors remove local/config-bucket

# encryption
s4 encrypt set local/config-bucket ./encryption.xml
s4 encrypt info local/config-bucket
s4 encrypt clear local/config-bucket

# events
s4 event add local/config-bucket ./notification.xml
s4 event ls local/config-bucket
s4 event rm local/config-bucket --force

# idp / ilm / replicate are placeholders that report "not implemented"
s4 idp openid
s4 ilm rule
s4 replicate ls local/config-bucket

s4 rb local/config-bucket

# legal hold and retention (object-lock bucket required)
s4 mb --with-lock local/lock-bucket
s4 put hello.txt local/lock-bucket/hello.txt
s4 legalhold set local/lock-bucket/hello.txt
s4 legalhold info local/lock-bucket/hello.txt
s4 legalhold clear local/lock-bucket/hello.txt
s4 retention set local/lock-bucket/hello.txt --mode GOVERNANCE --retain-until 2030-01-01T00:00:00Z
s4 retention info local/lock-bucket/hello.txt
s4 retention clear local/lock-bucket/hello.txt
# add --bypass if objects are still under governance retention
s4 rb --force local/lock-bucket
```

> `sql --csv-output-header <line>` prints the given header line once above the CSV records. `sql --enc-c` (SSE-C keys) is not implemented yet and is rejected with an explicit error.

> Note on rate limits: `--limit-upload` and `--limit-download` are passed to `curl` as `--limit-rate`; the upload limit applies to requests with a body, the download limit to all other requests (`get`, `cat`, `ls`, ...).

## Destructive operations

Like `mc`, `s4` never removes data behind your back:

- `s4 rb alias/bucket` fails on a non-empty bucket; `s4 rb --force alias/bucket` deletes every object version first.
- Governance-mode retention is only bypassed when asked: `s4 rm --bypass`, `s4 rb --force --bypass`, `s4 retention set --bypass` (to shorten a governance retention).
- `s4 retention clear` removes governance retention (empty retention with governance bypass, as `mc retention clear` does); compliance retention cannot be cleared.

## Mirror/sync flags (compatibility with `mc mirror`)

The source and destination prefixes are treated as directories: `alias/bucket/photos` covers `photos` and `photos/...`, but not `photos2/...`.
Objects that already exist on the destination with the same size and the same ETag (or a newer modification time) are skipped, so repeated runs and `--watch` passes only copy what changed. Within one account (same endpoint and access key) objects are copied server-side.

Supported in `s4 mirror`/`s4 sync`:
- `--dry-run`
- `--remove` (objects skipped by `--exclude` or the age filters are never removed from the destination)
- `--watch/-w` (polling mode; the default interval is 2s, configurable via `S4_SYNC_WATCH_INTERVAL_SEC`; after the first pass only passes that copied or removed something are reported)
- `--exclude <glob>` (can be given multiple times; `*` and `?` are supported; the pattern is matched against the full source key, e.g. `photos/*.tmp`, and `*` also matches `/`)
- `--newer-than <duration>` (only objects modified within the duration, e.g. `30m`, `12h`, `7d10h`; units `d/h/m/s`)
- `--older-than <duration>` (only objects modified earlier than the duration ago)
- `--overwrite` (accepted for compatibility; objects that differ from the source are always overwritten)

Not implemented yet — these return an explicit `not implemented yet` error:
- `--preserve/-a`, `--active-active`, `--disable-multipart`, `--exclude-bucket`,
  `--exclude-storageclass`, `--storage-class/--sc`, `--attr`,
  `--monitoring-address`, `--retry`, `--summary`, `--skip-errors`, `--max-workers`, `--checksum`,
  `--enc-c`, `--enc-kms`, `--enc-s3`, `--region` and other special `mc mirror` flags.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the code layout, the layers, the request lifecycle (signing, curl, response sinks), uploads and downloads, the sync algorithm, the design rules and how to add commands.

## Automated e2e

The `scripts/e2e.sh` script is provided for a full smoke/e2e run.

Example run against your own endpoint:

```bash
S4_E2E_ENDPOINT=http://63.141.251.44:10117 \
S4_E2E_ACCESS_KEY=my-secret-key_id \
S4_E2E_SECRET_KEY=my-secret-access-key \
S4_E2E_REGION=us-east-1 \
S4_E2E_PATH_STYLE=1 \
./scripts/e2e.sh
```

The script exercises `alias set/ls/rm`, `ls`, `mb`, `put`, `stat`, `cat`, `get`, `rm`, `rb` and verifies the integrity of the uploaded/downloaded content with `cmp`.

## Regression cases

`scripts/regression_cases.sh` runs one check per previously fixed bug (sync `--remove` with filters, downloads that must not clobber files, `rb --force`/`--bypass`, listings over 1000 keys, prefixes with `/`, valid `--json`, binary-safe `cat`, streaming `pipe`, signed `x-amz-*` headers, config permissions, temp-file cleanup, ...) and exits non-zero if any check fails. It reads `S4_E2E_ENDPOINT`, `S4_E2E_ACCESS_KEY`, `S4_E2E_SECRET_KEY` and optionally `S4_E2E_REGION`, like `e2e.sh`; set `S4_E2E_ENDPOINT2`, `S4_E2E_ACCESS_KEY2` and `S4_E2E_SECRET_KEY2` to also check `cp`/`sync` between two endpoints, and `S4_BIN` to test a prebuilt binary instead of `cargo build` output:

```bash
S4_E2E_ENDPOINT=http://127.0.0.1:9000 \
S4_E2E_ACCESS_KEY=minioadmin \
S4_E2E_SECRET_KEY=minioadmin123 \
./scripts/regression_cases.sh
```

## Server compatibility

`scripts/ci_s3_cases.sh` and `scripts/regression_cases.sh` pass against s4core (`s4-server`) and MinIO. Server-side gaps seen while testing:

- s4core: bucket notifications are not supported — `event add` and `event rm` fail with `409 BucketAlreadyExists` (the server treats `PUT /bucket?notification` as CreateBucket).
- MinIO: `cors` returns `501 NotImplemented`; `encrypt set` needs a configured KMS; `event add` needs an existing notification target ARN.

## CI

The repository includes the workflow `.github/workflows/ci.yml`, which runs:

- on every `push`
- on `pull_request` into `main`

The pipeline performs:

1. `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`
2. `cargo test --all-targets`
3. Integration S3 cases against a local MinIO (`scripts/ci_s3_cases.sh`), including `sync` and `mirror` (the mc-compatible alias), followed by `scripts/regression_cases.sh`.
4. On `push` to `main` — integration S3 cases against a remote endpoint taken from GitHub Secrets (with `S4_E2E_REMOTE_LIMITED=1`).

The integration scripts need `curl` (7.55+) and `python3` on the runner, like the CLI itself.

### Secrets for the remote S3 job

Add these under `Settings -> Secrets and variables -> Actions`:

- `S3_ENDPOINT` (for example `63.141.251.44`)
- `S3_ENDPOINT_PORT` (for example `10117`)
- `S3_ACCESS_KEY_ID` (for example `my-secret-key_id`)
- `S3_SECRET_ACCESS_KEY` (for example `my-secret-access-key`)


## Monitoring CI without clicking around

The `scripts/monitor_ci.sh` script uses the GitHub API to show the status and jobs of the `CI` workflow for the current commit/branch.

Examples:

```bash
# one-off check of the latest CI run for the current commit
GITHUB_TOKEN=... ./scripts/monitor_ci.sh --sha "$(git rev-parse HEAD)"

# wait until it finishes and return an exit code (0=success)
GITHUB_TOKEN=... ./scripts/monitor_ci.sh --wait --sha "$(git rev-parse HEAD)"

# wait and automatically rerun failed jobs
GITHUB_TOKEN=... ./scripts/monitor_ci.sh --wait --rerun-failed --sha "$(git rev-parse HEAD)"
```

If `GITHUB_REPOSITORY` is not set, the script determines `owner/repo` from the `origin` git remote on its own.


## Command coverage: mc vs s4

At this stage `s4` implements: `alias`, `ls`, `mb`, `rb`, `put`, `get`, `rm`, `stat`, `cat`, `cors`, `encrypt`, `event`, `legalhold`, `retention`, `sql`, `idp` (placeholder), `ilm` (placeholder), `replicate` (placeholder), `sync`, `mirror`, `cp`, `mv`, `find`, `tree`, `head`, `pipe`, `ping`, `ready`.

The remaining commands from the full `mc` list (for example `admin`, `anonymous`, `watch`, `tag`, etc.) are **not implemented yet** and require separate iterations.


## Flags: what exists and what does not yet

The currently supported global flags are: `-C/--config-dir`, `--json`, `--debug`, `--insecure`, `--resolve`, `--limit-upload`, `--limit-download`, `--custom-header/-H`, `-h/--help`, `-v/--version`.

Flags from `mc` that are not implemented yet: `--quiet`, `--disable-pager`, `--no-color`, `--autocompletion` and others.


> `idp openid|ldap` are currently added as placeholder commands (they return `not implemented`) for CLI compatibility; full integration with the MinIO admin API will be a separate stage.


> `ilm rule|tier|restore` are currently added as placeholder commands (they return `not implemented`) for CLI compatibility; a full lifecycle/tier/restore implementation will be a separate stage.


> `legalhold set|clear|info` are supported for objects in buckets with object-lock (use `mb --with-lock`).


> `replicate add|update|list|status|resync|export|import|remove|backlog` are currently added as placeholder commands (they return `not implemented`) for CLI compatibility; full server-side replication configuration will be a separate stage.


> `retention set|clear|info` are supported for objects in buckets with object-lock (use `mb --with-lock`).
