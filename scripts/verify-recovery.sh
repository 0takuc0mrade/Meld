#!/usr/bin/env bash
set -euo pipefail

meld_url="${MELD_URL:-http://127.0.0.1:3000}"
timeout_seconds="${MELD_WORKFLOW_TIMEOUT_SECONDS:-120}"
poll_seconds="${MELD_POLL_SECONDS:-1}"
proof_path="${MELD_PROOF_PATH:-}"
work_dir="$(mktemp -d)"
trap 'rm -r "$work_dir"' EXIT

start_path="$work_dir/start.json"
snapshot_path="$work_dir/snapshot.json"

curl --fail --silent --show-error \
  --request POST \
  "$meld_url/api/missions/demo" >"$start_path"

task_id="$(jq -er '.task_id' "$start_path")"
deadline="$((SECONDS + timeout_seconds))"

while (( SECONDS < deadline )); do
  curl --fail --silent --show-error \
    "$meld_url/api/tasks/$task_id" >"$snapshot_path"

  status="$(jq -er '.status.name' "$snapshot_path")"
  stale_count="$(jq '[.events[] | select(.kind == "submission.stale_rejected")] | length' "$snapshot_path")"

  if [[ "$status" == "failed" || "$stale_count" -gt 0 ]]; then
    break
  fi

  sleep "$poll_seconds"
done

status="$(jq -er '.status.name' "$snapshot_path")"
accepted_worker="$(jq -er '.accepted_result.worker_id' "$snapshot_path")"
accepted_generation="$(jq -er '.accepted_result.generation' "$snapshot_path")"
verification_count="$(jq '[.events[] | select(.kind == "verification.passed")] | length' "$snapshot_path")"
completion_count="$(jq '[.events[] | select(.kind == "task.completed")] | length' "$snapshot_path")"
stale_count="$(jq '[.events[] | select(.kind == "submission.stale_rejected" and .submitted_generation == 1 and .current_generation == 2)] | length' "$snapshot_path")"

[[ "$status" == "completed" ]]
[[ "$accepted_worker" == "Worker B" ]]
[[ "$accepted_generation" == "2" ]]
[[ "$verification_count" == "1" ]]
[[ "$completion_count" == "1" ]]
[[ "$stale_count" == "1" ]]

if [[ -n "$proof_path" ]]; then
  cp "$snapshot_path" "$proof_path"
fi

jq -r '
  "Meld recovery proof passed:",
  "  task_id=\(.task_id)",
  "  status=\(.status.name)",
  "  accepted_worker=\(.accepted_result.worker_id)",
  "  accepted_generation=\(.accepted_result.generation)",
  "  final_sequence=\(.current_sequence)"
' "$snapshot_path"
