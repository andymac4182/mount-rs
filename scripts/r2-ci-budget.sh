#!/bin/sh
set -eu

# GitHub Actions has no provider-side R2 spending ceiling. This admission gate
# bounds how many live-R2 workflow runs this repository may accept in one
# billing month and fails closed if the GitHub Actions API is unavailable.
# Cloudflare's separate account-wide budget alert is the notification backstop.

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set}"
: "${GH_TOKEN:?GH_TOKEN must be set}"

workflow_file=${MOUNT_RS_R2_CI_WORKFLOW_FILE:-cloudflare-r2.yml}
run_limit=${MOUNT_RS_R2_CI_MAX_RUNS_PER_MONTH:-20}
per_run_cents=${MOUNT_RS_R2_CI_MAX_COST_CENTS_PER_RUN:-400}
monthly_budget_cents=${MOUNT_RS_R2_CI_MONTHLY_BUDGET_CENTS:-10000}

case "$run_limit:$per_run_cents:$monthly_budget_cents" in
  *[!0-9:]*|*:|:*|*::*|"")
    echo "R2 CI budget values must be non-negative integers" >&2
    exit 2
    ;;
esac

envelope_cents=$((run_limit * per_run_cents))
if [ "$envelope_cents" -gt "$monthly_budget_cents" ]; then
  echo "R2 CI cost envelope exceeds its monthly budget" >&2
  echo "runs=$run_limit per_run_cents=$per_run_cents monthly_budget_cents=$monthly_budget_cents" >&2
  exit 2
fi
envelope_usd=$(awk "BEGIN { printf \"%.2f\", $envelope_cents / 100 }")

month_start=$(date -u +%Y-%m-01T00:00:00Z)
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
run_count=$(gh api \
  --method GET \
  --header 'Accept: application/vnd.github+json' \
  --header 'X-GitHub-Api-Version: 2022-11-28' \
  "repos/$GITHUB_REPOSITORY/actions/workflows/$workflow_file/runs" \
  -f branch=main \
  -f created="${month_start}..${now}" \
  -f per_page=1 \
  --jq '.total_count')

case "$run_count" in
  ''|*[!0-9]*)
    echo "GitHub returned an invalid live-R2 workflow run count" >&2
    exit 2
    ;;
esac

if [ "$run_count" -gt "$run_limit" ]; then
  echo "R2 CI monthly run cap already exceeded: count=$run_count limit=$run_limit" >&2
  exit 1
fi

if [ "$run_count" -eq "$run_limit" ]; then
  echo "R2 CI monthly run cap reached: count=$run_count limit=$run_limit" >&2
  exit 1
fi

accepted_count=$((run_count + 1))
remaining=$((run_limit - accepted_count))
printf 'R2_CI_BUDGET_PASS month=%s accepted_run=%s/%s estimated_monthly_envelope_usd=$%s remaining_slots=%s\n' \
  "${month_start%%T*}" \
  "$accepted_count" \
  "$run_limit" \
  "$envelope_usd" \
  "$remaining"

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    printf 'accepted_run=%s\n' "$accepted_count"
    printf 'run_limit=%s\n' "$run_limit"
    printf 'estimated_monthly_envelope_usd=%s\n' "$envelope_usd"
    printf 'remaining_slots=%s\n' "$remaining"
  } >>"$GITHUB_OUTPUT"
fi

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    printf '%s\n' '### Live R2 CI budget guard'
    printf '%s\n' ''
    printf '%s\n' "- Accepted run: $accepted_count/$run_limit this billing month"
    printf '%s\n' "- Worst-case lane envelope: \$$envelope_usd per month"
    printf '%s\n' "- Remaining run slots: $remaining"
  } >>"$GITHUB_STEP_SUMMARY"
fi
