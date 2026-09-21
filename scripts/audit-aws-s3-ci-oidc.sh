#!/bin/sh
set -eu

# Read-only audit for the GitHub environment and AWS role used by the live
# AWS S3 qualification workflow. This script intentionally never changes IAM,
# GitHub environments, variables, or secrets, and never prints a secret value.

REPOSITORY=${AWS_S3_CI_GITHUB_REPOSITORY:-andymac4182/mount-rs}
ENVIRONMENT=${AWS_S3_CI_GITHUB_ENVIRONMENT:-aws-s3-ci}
ROLE_ARN=${AWS_S3_CI_ROLE_ARN:-}
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

blocked=""

block() {
  blocked="$blocked $1"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "AWS_S3_OIDC_AUDIT_BLOCKED missing_command_$1" >&2
    exit 2
  }
}

has_name() {
  command_output=$1
  name=$2
  printf '%s\n' "$command_output" | grep -Fqx "$name"
}

require_command gh
require_command aws
require_command jq
require_command grep

case "$REPOSITORY" in
  */*) ;;
  *) block invalid_repository_shape ;;
esac

case "$ENVIRONMENT" in
  ''|*[!A-Za-z0-9_.-]*) block invalid_environment_shape ;;
esac

case "$ROLE_ARN" in
  arn:aws:iam::????????????:role/*) ;;
  *) block invalid_role_arn_shape ;;
esac

repo_json=$(gh api "repos/$REPOSITORY" 2>/dev/null || true)
if [ -z "$repo_json" ]; then
  block github_repository_unreadable
else
  owner_login=$(printf '%s' "$repo_json" | jq -r '.owner.login // empty')
  owner_id=$(printf '%s' "$repo_json" | jq -r '.owner.id // empty')
  repo_name=$(printf '%s' "$repo_json" | jq -r '.name // empty')
  repo_id=$(printf '%s' "$repo_json" | jq -r '.id // empty')
  case "$owner_login:$owner_id:$repo_name:$repo_id" in
    *:*) ;;
    *) block github_repository_metadata_missing ;;
  esac
  case "$owner_id:$repo_id" in
    ''|*[!0-9:]*|*:*[!0-9]*) block github_repository_id_invalid ;;
  esac
fi

environment_json=$(gh api "repos/$REPOSITORY/environments/$ENVIRONMENT" 2>/dev/null || true)
if [ -z "$environment_json" ]; then
  block github_environment_unreadable
else
  printf '%s' "$environment_json" | jq -e '.protection_rules | length > 0' >/dev/null 2>&1 ||
    block environment_missing_protection_rule
  printf '%s' "$environment_json" |
    jq -e '.deployment_branch_policy.protected_branches == true' >/dev/null 2>&1 ||
    block environment_missing_protected_branch_policy
  printf '%s' "$environment_json" |
    jq -e -f "$repo_dir/scripts/check-aws-s3-ci-environment.jq" >/dev/null 2>&1 ||
    block environment_missing_non_self_review_required_reviewer
fi

variables=$(gh variable list --repo "$REPOSITORY" --env "$ENVIRONMENT" --json name --jq '.[].name' 2>/dev/null || true)
for variable in \
  MOUNT_RS_AWS_S3_TEST_BUCKET \
  MOUNT_RS_AWS_S3_TEST_REGION \
  MOUNT_RS_AWS_S3_ACCOUNT_ID \
  MOUNT_RS_AWS_S3_TEST_VERSIONING_STATUS
do
  has_name "$variables" "$variable" || block "missing_environment_variable_$variable"
done

secrets=$(gh secret list --repo "$REPOSITORY" --env "$ENVIRONMENT" --json name --jq '.[].name' 2>/dev/null || true)
has_name "$secrets" MOUNT_RS_AWS_S3_CI_ROLE_ARN ||
  block missing_environment_secret_MOUNT_RS_AWS_S3_CI_ROLE_ARN

if [ -n "$ROLE_ARN" ]; then
  role_name=${ROLE_ARN##*/}
  role_json=$(aws iam get-role --role-name "$role_name" --query Role --output json 2>/dev/null || true)
  if [ -z "$role_json" ]; then
    block aws_role_unreadable
  else
    actual_role_arn=$(printf '%s' "$role_json" | jq -r '.Arn // empty')
    [ "$actual_role_arn" = "$ROLE_ARN" ] || block aws_role_arn_mismatch

    account_id=${ROLE_ARN#arn:aws:iam::}
    account_id=${account_id%%:role/*}
    provider_arn="arn:aws:iam::$account_id:oidc-provider/token.actions.githubusercontent.com"
    provider_json=$(aws iam get-open-id-connect-provider \
      --open-id-connect-provider-arn "$provider_arn" --output json 2>/dev/null || true)
    if [ -z "$provider_json" ]; then
      block missing_github_oidc_provider
    else
      printf '%s' "$provider_json" |
        jq -e '.ClientIDList | index("sts.amazonaws.com") != null' >/dev/null 2>&1 ||
        block oidc_provider_missing_sts_audience
      printf '%s' "$provider_json" |
        jq -e '.Url == "https://token.actions.githubusercontent.com"' >/dev/null 2>&1 ||
        block oidc_provider_url_mismatch
      printf '%s' "$provider_json" |
        jq -e '.ThumbprintList | length > 0' >/dev/null 2>&1 ||
        block oidc_provider_missing_thumbprint
    fi

    if [ -n "${owner_login:-}" ] && [ -n "${owner_id:-}" ] &&
      [ -n "${repo_name:-}" ] && [ -n "${repo_id:-}" ]; then
      expected_subject="repo:$owner_login@$owner_id/$repo_name@$repo_id:environment:$ENVIRONMENT"
      trust_policy=$(printf '%s' "$role_json" | jq -c '.AssumeRolePolicyDocument // {}')
      printf '%s' "$trust_policy" | jq -e \
        --arg provider "$provider_arn" \
        --arg subject "$expected_subject" '
          def includes($value):
            if type == "array" then index($value) != null else . == $value end;
          def as_array:
            if type == "array" then . else [.] end;
          [
            .Statement[]?
            | select(
                .Effect == "Allow"
                and ((.Principal.Federated // empty) | includes($provider))
              )
          ] as $github_statements
          | ($github_statements | length) == 1
          and (
            $github_statements[0] as $statement
            | (($statement.Principal | keys | sort) == ["Federated"])
            and $statement.Principal.Federated == $provider
            and (($statement.Action | as_array | sort) == ["sts:AssumeRoleWithWebIdentity"])
            and $statement.Condition.StringEquals["token.actions.githubusercontent.com:aud"] == "sts.amazonaws.com"
            and $statement.Condition.StringEquals["token.actions.githubusercontent.com:sub"] == $subject
          )' >/dev/null 2>&1 || block role_missing_immutable_github_subject_trust
    else
      block github_subject_metadata_missing
    fi
  fi
fi

if [ -n "$blocked" ]; then
  echo "AWS_S3_OIDC_AUDIT_BLOCKED$blocked" >&2
  exit 2
fi

echo "AWS_S3_OIDC_AUDIT_PASS repository=$REPOSITORY environment=$ENVIRONMENT role=redacted subject=immutable"
