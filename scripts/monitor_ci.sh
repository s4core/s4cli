#!/usr/bin/env bash
set -euo pipefail

# Monitor GitHub Actions CI workflow runs for current repository.
#
# Requirements:
#   - GITHUB_TOKEN with repo/actions read permissions (and actions:write for rerun)
#   - git remote 'origin' configured or GITHUB_REPOSITORY set as owner/repo
#
# Usage:
#   scripts/monitor_ci.sh [--wait] [--interval 10] [--workflow CI] [--branch main] [--sha <commit>] [--rerun-failed]

WAIT=0
INTERVAL=10
WORKFLOW_NAME="CI"
BRANCH=""
SHA=""
RERUN_FAILED=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --wait)
      WAIT=1
      shift
      ;;
    --interval)
      INTERVAL="$2"
      shift 2
      ;;
    --workflow)
      WORKFLOW_NAME="$2"
      shift 2
      ;;
    --branch)
      BRANCH="$2"
      shift 2
      ;;
    --sha)
      SHA="$2"
      shift 2
      ;;
    --rerun-failed)
      RERUN_FAILED=1
      shift
      ;;
    *)
      echo "Unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "${GITHUB_TOKEN:-}" ]]; then
  echo "GITHUB_TOKEN is required" >&2
  exit 2
fi

resolve_repo() {
  if [[ -n "${GITHUB_REPOSITORY:-}" ]]; then
    echo "$GITHUB_REPOSITORY"
    return
  fi

  local url
  url="$(git config --get remote.origin.url || true)"
  if [[ -z "$url" ]]; then
    echo "Cannot detect repository. Set GITHUB_REPOSITORY=owner/repo" >&2
    exit 2
  fi

  # supports git@github.com:owner/repo.git and https://github.com/owner/repo.git
  local repo
  repo="$(python3 - "$url" <<'PY'
import re,sys
u=sys.argv[1]
m=re.search(r'github\.com[:/](.+?)(?:\.git)?$', u)
print(m.group(1) if m else '')
PY
)"
  if [[ -z "$repo" ]]; then
    echo "Failed to parse owner/repo from origin URL: $url" >&2
    exit 2
  fi
  echo "$repo"
}

if [[ -z "$BRANCH" ]]; then
  BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
fi
if [[ -z "$SHA" ]]; then
  SHA="$(git rev-parse HEAD 2>/dev/null || true)"
fi

REPO="$(resolve_repo)"
API_BASE="https://api.github.com/repos/${REPO}"
AUTH=(-H "Authorization: Bearer ${GITHUB_TOKEN}" -H "Accept: application/vnd.github+json" -H "X-GitHub-Api-Version: 2022-11-28")

fetch_runs_json() {
  local url="${API_BASE}/actions/runs?per_page=50"
  if [[ -n "$BRANCH" ]]; then
    url+="&branch=${BRANCH}"
  fi
  curl -fsSL "${AUTH[@]}" "$url"
}

select_run() {
  python3 - "$WORKFLOW_NAME" "$SHA" <<'PY'
import json,sys
workflow_name=sys.argv[1]
sha=sys.argv[2]
data=json.load(sys.stdin)
runs=data.get("workflow_runs", [])
for run in runs:
    if run.get("name") != workflow_name:
        continue
    if sha and run.get("head_sha") != sha:
        continue
    print(json.dumps({
        "id": run.get("id"),
        "html_url": run.get("html_url"),
        "status": run.get("status"),
        "conclusion": run.get("conclusion"),
        "head_branch": run.get("head_branch"),
        "head_sha": run.get("head_sha"),
        "run_number": run.get("run_number"),
    }))
    break
PY
}

print_run() {
  python3 - <<'PY'
import json,sys
run=json.load(sys.stdin)
print(f"run #{run['run_number']} id={run['id']} status={run['status']} conclusion={run['conclusion']}")
print(f"branch={run['head_branch']} sha={run['head_sha']}")
print(f"url={run['html_url']}")
PY
}

print_jobs() {
  local run_id="$1"
  curl -fsSL "${AUTH[@]}" "${API_BASE}/actions/runs/${run_id}/jobs?per_page=100" | python3 - <<'PY'
import json,sys
jobs=json.load(sys.stdin).get("jobs", [])
for j in jobs:
    print(f"  - {j.get('name')}: status={j.get('status')} conclusion={j.get('conclusion')}")
PY
}

rerun_failed_jobs() {
  local run_id="$1"
  curl -fsSL -X POST "${AUTH[@]}" "${API_BASE}/actions/runs/${run_id}/rerun-failed-jobs" >/dev/null
}

echo "[ci-monitor] repo=${REPO} workflow=${WORKFLOW_NAME} branch=${BRANCH:-<any>} sha=${SHA:-<latest>}"

while true; do
  run_json="$(fetch_runs_json | select_run || true)"
  if [[ -z "$run_json" ]]; then
    echo "[ci-monitor] No matching run yet."
    if [[ "$WAIT" == "1" ]]; then
      sleep "$INTERVAL"
      continue
    fi
    exit 1
  fi

  echo "$run_json" | print_run
  run_id="$(echo "$run_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
  status="$(echo "$run_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')"
  conclusion="$(echo "$run_json" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("conclusion"))')"

  print_jobs "$run_id"

  if [[ "$status" == "completed" ]]; then
    if [[ "$conclusion" == "success" ]]; then
      echo "[ci-monitor] CI is successful."
      exit 0
    fi

    echo "[ci-monitor] CI completed with conclusion=${conclusion}."
    if [[ "$RERUN_FAILED" == "1" ]]; then
      echo "[ci-monitor] rerunning failed jobs..."
      rerun_failed_jobs "$run_id"
      sleep "$INTERVAL"
      continue
    fi
    exit 1
  fi

  if [[ "$WAIT" == "1" ]]; then
    sleep "$INTERVAL"
    continue
  fi

  exit 0
done
