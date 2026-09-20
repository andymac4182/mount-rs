# Actual AWS S3 integration

This is the opt-in W25 actual-service gate. It is separate from the
Cloudflare R2 and RustFS lanes: the Rust test builds the existing
`mount-rs-r2::R2BlockStore` over the AWS S3 client, then composes it with the
production `ChunkedFs` and durable local SQLite metadata.

The gate covers:

- create-only immutable block publication and duplicate conditional-write
  rejection;
- an actual S3 byte range read and filesystem offset reads;
- multi-chunk writes, partial overwrite, truncate/extend, and a sparse tail;
- fresh metadata and fresh signed S3-client reopen;
- a second Cargo test process reading a block left by the first process; and
- shell-owned cleanup verified to leave no current objects below the run
  prefix.

The tests are ignored in ordinary Cargo runs. The harness never creates an IAM
user, access key, bucket, policy, or endpoint. It accepts credentials already
present in `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` (and optional
`AWS_SESSION_TOKEN`), or exports temporary credentials from `AWS_PROFILE` using
the AWS CLI credential chain. Secrets are not printed or written to the
repository.

## External prerequisites

Use a dedicated private S3 bucket. The W25 configuration recorded for this
repository is:

- bucket: `mount-rs-integration-106427005394-ap-southeast-2`;
- region: `ap-southeast-2`;
- all four S3 Block Public Access settings enabled;
- Bucket owner enforced object ownership;
- SSE-S3 (`AES256`) default encryption;
- lifecycle expiration for objects below `mount-rs-tests/` after seven days;
- incomplete multipart uploads aborted after one day; and
- no access-key creation as part of this test setup.

The test credential must already be authenticated outside the repository. A
narrow role/user policy needs, at minimum, `sts:GetCallerIdentity`,
`s3:ListBucket` restricted by prefix to `mount-rs-tests/aws-s3/*`, and
`s3:GetObject`, `s3:PutObject`, and `s3:DeleteObject` restricted to the same
object ARN prefix. The bucket and prefix must be owned by the test account;
the harness does not create or discover resources.

If the selected profile uses SSO, refresh its session or use another
already-authorized profile before running the gate. Do not paste credentials
or session tokens into chat or source control.

## Run

The explicit opt-in is required:

```sh
AWS_PROFILE=myroot \
AWS_S3_TEST_BUCKET=mount-rs-integration-106427005394-ap-southeast-2 \
AWS_S3_TEST_REGION=ap-southeast-2 \
MOUNT_RS_RUN_AWS_S3=1 \
./scripts/test-aws-s3.sh
```

If the profile uses SSO, authenticate it with the normal AWS CLI flow before
the command. The script calls `aws configure export-credentials` privately,
preflights STS and the named bucket, refuses custom/local endpoints, and
generates a unique prefix below `mount-rs-tests/aws-s3/`. You may provide an
already reserved run prefix with `MOUNT_RS_AWS_S3_TEST_PREFIX`, but it must be
below that root and empty; a non-empty prefix is refused rather than deleted.

The script removes only `s3://$AWS_S3_TEST_BUCKET/$MOUNT_RS_AWS_S3_TEST_PREFIX/`
and verifies that `ListObjectsV2` returns zero current objects. It does not
delete the bucket or any object outside the exact run prefix. An invocation
without `MOUNT_RS_RUN_AWS_S3=1` reports `AWS_S3_TEST_SKIPPED`; an opted-in run
with missing/expired credentials fails with a prerequisite error and is not a
pass.
