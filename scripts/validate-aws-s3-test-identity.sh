#!/bin/sh
set -eu

# Compare the authenticated caller account with the approved W25 account.
# The AWS CLI performs the identity lookup; this helper is deliberately pure
# so it can be regression-tested without credentials or network access.

fail() {
  echo "AWS_S3_TEST_IDENTITY_BLOCKED $*" >&2
  exit 2
}

expected=${AWS_S3_TEST_EXPECTED_ACCOUNT_ID:-}
caller=${AWS_S3_TEST_CALLER_ACCOUNT_ID:-}

[ -n "$expected" ] || fail missing_expected_account_id
[ -n "$caller" ] || fail missing_caller_account_id

case "$expected" in
  ''|*[!0-9]*) fail invalid_expected_account_id_shape ;;
esac
[ "${#expected}" -eq 12 ] || fail invalid_expected_account_id_length

case "$caller" in
  ''|*[!0-9]*) fail invalid_caller_account_id_shape ;;
esac
[ "${#caller}" -eq 12 ] || fail invalid_caller_account_id_length

[ "$caller" = "$expected" ] || fail caller_account_mismatch

echo "AWS_S3_TEST_IDENTITY_PASS account=$expected"
