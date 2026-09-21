#!/bin/sh
set -eu

# Credential-free regression tests for the protected GitHub environment
# contract. These fixtures contain no repository, user, or secret values.

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
filter=$repo_dir/scripts/check-aws-s3-ci-environment.jq

valid='{"protection_rules":[{"type":"required_reviewers","prevent_self_review":true,"reviewers":[{"type":"User"}]}],"deployment_branch_policy":{"protected_branches":true}}'
missing_reviewers='{"protection_rules":[{"type":"wait_timer","wait_timer":30}],"deployment_branch_policy":{"protected_branches":true}}'
self_review_allowed='{"protection_rules":[{"type":"required_reviewers","prevent_self_review":false,"reviewers":[{"type":"User"}]}],"deployment_branch_policy":{"protected_branches":true}}'

fail() {
  echo "AWS_S3_CI_ENVIRONMENT_TEST_FAILED $*" >&2
  exit 1
}

printf '%s' "$valid" | jq -e -f "$filter" >/dev/null ||
  fail valid_environment_rejected

if printf '%s' "$missing_reviewers" | jq -e -f "$filter" >/dev/null; then
  fail missing_reviewers_accepted
fi

if printf '%s' "$self_review_allowed" | jq -e -f "$filter" >/dev/null; then
  fail self_review_allowed
fi

echo "AWS_S3_CI_ENVIRONMENT_TEST_PASS cases=3"
