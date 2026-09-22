# Actual AWS S3 integration

This is the opt-in W25 actual-service gate. It is separate from the
Cloudflare R2 and RustFS lanes: the Rust test builds the first-class
`mount-rs-sdk::Filesystem` with `StoreConfig::AwsS3`, then composes it with
durable local SQLite metadata. The harness also runs the real Rust CLI
`sdk-self-test` against the same configuration and scoped credentials.

The gate covers:

- create-only immutable block publication, duplicate conditional-create
  rejection, stale conditional reads/writes, and a successful ETag CAS probe;
- an actual S3 byte range read and filesystem offset reads;
- multi-chunk writes, partial overwrite, truncate/extend, and a sparse tail;
- fresh metadata and fresh signed S3-client reopen;
- a second Cargo test process reading a block and composed SQLite+S3
  filesystem left by the first process; and
- the CLI's public configuration path writing, reopening, and cleaning a
  separate AWS S3 prefix; and
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

- bucket: `mount-rs-integration-922978963556-ap-southeast-2`;
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

For a short-lived least-privilege role, set `AWS_S3_TEST_ROLE_ARN` alongside
`AWS_PROFILE` (or explicit base credentials) and set
`AWS_S3_TEST_EXPECTED_ACCOUNT_ID` to the approved 12-digit account. The
harness obtains temporary role credentials with `sts:AssumeRole` before the
preflight, requires the role ARN account to match that expected account, and
uses the resulting credentials for the Rust tests and cleanup. It never creates
or modifies the role, policy, bucket, or access keys.

For the recorded bucket, the identity policy can be scoped to the harness's
dedicated test namespace (replace the bucket ARN if the test bucket changes;
for stricter per-run isolation, replace `mount-rs-tests/aws-s3/*` with the
concrete reserved run prefix in both statements):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "ListOnlyMountRsTestPrefix",
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::mount-rs-integration-922978963556-ap-southeast-2",
      "Condition": {
        "StringLike": {
          "s3:prefix": "mount-rs-tests/aws-s3/*"
        }
      }
    },
    {
      "Sid": "ObjectsOnlyMountRsTestPrefix",
      "Effect": "Allow",
      "Action": [
        "s3:DeleteObject",
        "s3:GetObject",
        "s3:PutObject"
      ],
      "Resource": "arn:aws:s3:::mount-rs-integration-922978963556-ap-southeast-2/mount-rs-tests/aws-s3/*"
    },
    {
      "Sid": "IdentifyTestPrincipal",
      "Effect": "Allow",
      "Action": "sts:GetCallerIdentity",
      "Resource": "*"
    }
  ]
}
```

The runner uses prefix-scoped `ListObjectsV2` for both the empty-prefix
preflight and cleanup, so it does not require broad bucket listing or
`HeadBucket`. Each run includes a random ownership nonce in its default
prefix, claims `<prefix>/.mount-rs-run-owner` with an atomic create, and
verifies that marker before cleanup. A concurrent run using the same explicit
prefix therefore fails closed instead of deleting the other run's objects;
only pass an explicit prefix that this test owns. AWS CLI endpoint and service
profile overrides are refused. Use a short-lived SSO or assumed-role session
where possible; the runner never creates access keys, prints credential
values, or writes them to the repository.

After role assumption, the harness also attempts a non-mutating list under a
sibling `mount-rs-tests/aws-s3-denied/` prefix and requires AWS authorization
failure. A successful service run therefore proves both the required
prefix-scoped operations and the configured role's list boundary.

Before assuming the prefix-scoped role, the harness reads the bucket's
versioning state with the audit identity. A separated hosted audit identity may
instead provide the reviewed `None`, `Enabled`, or `Suspended` value through
`AWS_S3_TEST_VERSIONING_STATUS`; an absent or invalid value blocks the run.
When versioning is enabled or suspended, cleanup enumerates and removes both
object versions and delete markers, and the cleanup identity must have the
corresponding version-delete permission.

The corresponding versioned CLI provider shape is:

```json
{
  "version": 1,
  "driver": {
    "kind": "splitstore",
    "storage": {
      "metadata": {"kind": "sqlite", "path": "./state/metadata.sqlite"},
      "blocks": {
        "kind": "aws-s3",
        "bucket": "private-bucket",
        "region": "ap-southeast-2",
        "prefix": "mount-rs/volume-a",
        "durable": true
      },
      "chunk_size_bytes": 65536,
      "owner": "volume-a"
    }
  }
}
```

When the AWS harness has already assumed the scoped test role, set
`MOUNT_RS_RUN_AWS_S3_PGLITE=1` to add the external PGlite metadata pairing
rows. The parent run starts the pinned PGlite socket server with a temporary
on-disk data directory, passes its exported short-lived AWS credentials to the
Rust tests, snapshots and restores that directory, and removes the child
prefix during the same version-aware cleanup:

```sh
MOUNT_RS_RUN_AWS_S3=1 \
MOUNT_RS_RUN_AWS_S3_PGLITE=1 \
AWS_PROFILE=myroot \
AWS_S3_TEST_ROLE_ARN=arn:aws:iam::922978963556:role/mount-rs/mount-rs-aws-s3-integration-test \
AWS_S3_TEST_EXPECTED_ACCOUNT_ID=922978963556 \
AWS_S3_TEST_BUCKET=mount-rs-integration-922978963556-ap-southeast-2 \
AWS_S3_TEST_REGION=ap-southeast-2 \
./scripts/test-aws-s3.sh
```

The first pairing row proves an actual AWS S3 block store can reopen with
fresh PGlite metadata and a fresh signed client. The persistent rows then
exercise independent-writer fencing, close the metadata clients, restore the
PGlite data directory into a fresh server process, and reopen the same AWS
objects. This is a local metadata backup/restore and provider-pairing
qualification lane; it is not approval of PGlite as production metadata, an
independent backup system, a multi-region disaster-recovery result, or a
production SLO result.

The AWS provider accepts credentials through the `object_store` workload
chain (environment credentials, web identity, ECS task credentials, or EC2
instance credentials). It intentionally has no endpoint or static-secret
fields; use the `r2` provider for an S3-compatible endpoint.

If the selected profile uses SSO, refresh its session or use another
already-authorized profile before running the gate. Do not paste credentials
or session tokens into chat or source control.

## Run

The explicit opt-in is required:

```sh
AWS_PROFILE=myroot \
AWS_S3_TEST_ROLE_ARN=arn:aws:iam::922978963556:role/mount-rs/mount-rs-aws-s3-integration-test \
AWS_S3_TEST_EXPECTED_ACCOUNT_ID=922978963556 \
AWS_S3_TEST_BUCKET=mount-rs-integration-922978963556-ap-southeast-2 \
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
and verifies that `ListObjectsV2` returns zero current objects. For versioned
buckets it also verifies zero object versions and delete markers. It does not
delete the bucket or any object outside the exact run prefix. An invocation
without `MOUNT_RS_RUN_AWS_S3=1` reports `AWS_S3_TEST_SKIPPED`; an opted-in run
whose local credential chain cannot authenticate reports
`AWS_S3_TEST_BLOCKED reason=local_cli_credentials_unavailable` (or the
corresponding local identity reason) and exits 3. The harness cannot inherit
credentials held by AWS MCP/OAuth, so this is a blocked prerequisite rather
than a live Rust-service pass.
