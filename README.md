# s4

`s4` — a CLI tool for S3/MinIO written in Rust, in the style of `mc`.

## What is implemented

- Global flags: `-C/--config-dir`, `--json`, `--debug`, `--insecure`, `--resolve`, `--limit-upload`, `--limit-download`, `--custom-header/-H`.
- Alias management: `alias set|ls|rm`.
- S3 commands: `ls`, `mb`, `rb`, `put`, `get`, `rm`, `stat`, `cat`, `cors`, `encrypt`, `event`, `legalhold`, `retention`, `sql`, `idp`, `ilm`, `replicate`, `sync`, `mirror` (alias for `sync`), `cp`, `mv`, `find`, `tree`, `head`, `pipe`, `ping`, `ready`.
- AWS SigV4 request signing is implemented through a built-in Python helper (`python3`), with HTTP calls made via `curl`.
- Large uploads (over 16 MiB) use multipart upload (`put`, `cp` local->s3, `sync/mirror`, `pipe`).
- Config format: `~/.s4/config.toml`.

> The current build only supports aliases with `--path-style`.

## Quick start

```bash
s4 alias set local http://127.0.0.1:9000 minio minio123 --path-style
s4 mb local/test-bucket
echo hello > hello.txt
s4 put hello.txt local/test-bucket/hello.txt
s4 cat local/test-bucket/hello.txt
s4 get local/test-bucket/hello.txt ./downloaded.txt
s4 stat local/test-bucket/hello.txt

# cors
s4 cors set local/test-bucket ./cors.xml
s4 cors get local/test-bucket
s4 cors remove local/test-bucket

# encryption
s4 encrypt set local/test-bucket ./encryption.xml
s4 encrypt info local/test-bucket
s4 encrypt clear local/test-bucket

# events
s4 event add local/test-bucket ./notification.xml
s4 event ls local/test-bucket
s4 event rm local/test-bucket --force

# legal hold (object-lock bucket required)
s4 mb --with-lock local/lock-bucket
s4 legalhold set local/lock-bucket/hello.txt
s4 legalhold info local/lock-bucket/hello.txt
s4 legalhold clear local/lock-bucket/hello.txt

# retention (object-lock bucket required)
s4 retention set local/lock-bucket/hello.txt --mode GOVERNANCE --retain-until 2030-01-01T00:00:00Z
s4 retention info local/lock-bucket/hello.txt
s4 retention clear local/lock-bucket/hello.txt

# idp (placeholder in current build)
s4 idp openid
s4 idp ldap

# ilm (placeholder in current build)
s4 ilm rule
s4 ilm tier
s4 ilm restore

# replicate (placeholder in current build)
s4 replicate ls local/test-bucket
s4 replicate status local/test-bucket

s4 rm local/test-bucket/hello.txt
s4 rb local/test-bucket

# synchronization (equivalent of mc mirror)
s4 sync local/source-bucket local/destination-bucket
# or in mc style
s4 mirror local/source-bucket local/destination-bucket

# copy / move
s4 cp ./local.txt local/test-bucket/local.txt
s4 cp local/test-bucket/local.txt ./local-copy.txt
s4 mv local/test-bucket/local.txt local/test-bucket/local-moved.txt

# find / tree / head
s4 find local/test-bucket photos
s4 tree local/test-bucket
s4 head local/test-bucket/local-moved.txt 5

# upload from stdin
echo "stream data" | s4 pipe local/test-bucket/stdin.txt

# checks
s4 ping local
s4 ready local

# sql select (S3 Select API)
s4 sql --query "select * from S3Object" local/test-bucket/data.csv
s4 sql -r --query "select count(*) from S3Object" local/test-bucket/reports/
```




> Note on rate limits: in the current implementation `--limit-upload` and `--limit-download` are passed to `curl` as `--limit-rate` for upload and download requests respectively.

## Mirror/sync flags (compatibility with `mc mirror`)

Supported in `s4 mirror`/`s4 sync`:
- `--dry-run`
- `--remove`
- `--watch/-w` (polling mode; the default interval is 2s, configurable via `S4_SYNC_WATCH_INTERVAL_SEC`)
- `--exclude <glob>` (can be given multiple times; `*` and `?` are supported)
- `--newer-than <duration>`
- `--older-than <duration>`
- `--overwrite` (accepted for compatibility; the current behavior already overwrites target objects)

Not implemented yet — these return an explicit `not implemented yet` error:
- `--preserve/-a`, `--active-active`, `--disable-multipart`, `--exclude-bucket`,
  `--exclude-storageclass`, `--storage-class/--sc`, `--attr`,
  `--monitoring-address`, `--retry`, `--summary`, `--skip-errors`, `--max-workers`, `--checksum`,
  `--enc-c`, `--enc-kms`, `--enc-s3`, `--region` and other special `mc mirror` flags.

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


## CI

The repository includes the workflow `.github/workflows/ci.yml`, which runs:

- on every `push`
- on `pull_request` into `main`

The pipeline performs:

1. `cargo fmt --all --check`
2. `cargo test --all-targets`
3. Integration S3 cases against a local MinIO (`scripts/ci_s3_cases.sh`), including `sync` and `mirror` (the mc-compatible alias).
4. On `push` to `main` — integration S3 cases against a remote endpoint taken from GitHub Secrets.

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
