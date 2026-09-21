# AWS S3 production rollout

This runbook tracks the production qualification boundary for the W25 AWS S3
block provider. It is deliberately separate from the provider implementation:
AWS S3 stores immutable blocks, while namespace metadata, leases, revisions,
and chunk references remain in an independently selected metadata provider.

The W25 test bucket and assumed role are qualification resources only. A green
W25 service run is not permission to promote that bucket, its SQLite metadata,
or its test configuration into a production workload.

## Readiness rule

The workstream is production-ready only when every gate below has a named
owner, current evidence, and a reviewed rollback path. Local tests, a demo,
an API probe, a planning document, or a test-account bucket cannot substitute
for a production deployment result.

| Gate | Required evidence | Status |
| --- | --- | --- |
| Provider contract | `kind: "aws-s3"` uses the real region, signed AWS workload credentials, immutable create, conditional update, and block-only semantics | Passed in local code gates and the live public SDK/CLI run |
| AWS resource controls | Reviewable IaC or equivalent, private bucket, Block Public Access, Object Ownership, encryption/KMS decision, lifecycle/versioning decision, and prefix ownership | Reviewable CloudFormation contract added and AWS syntax-validated; production parameters, change set, and resource review open |
| Identity | Runtime and maintenance roles are least-privilege, short-lived, trusted only by the intended workload, and have no committed access keys | Test role and sibling-prefix denial passed; production workload identity open |
| Metadata | A durable, independently operated metadata provider is selected and qualified for the intended host/multi-writer scope | Partial AWS S3 plus external PGlite qualification passed; production provider selection, multi-writer, backup/restore, and failure-recovery evidence remain open |
| Recovery | Backup/restore, schema migration, orphan-block cleanup, restart, failure recovery, and disaster-recovery drills pass | Open |
| Operations | S3 latency/error/retry/conditional-conflict signals, credential-expiry detection, cost/retention alerts, SLOs, and incident runbooks exist | Open; the S3 gateway now exposes a bounded `S3Session::stats()` snapshot for latency, buffered bytes, operation counts, and authentication/conditional/throttling/client/server error classes. The public SDK's optional observability path records provider block latency, errors, bytes, and reconciliation scanned/protected/recent/deleted counts through local snapshots, tracing, and OTLP counters. [`docs/aws-s3-operations-runbook.md`](aws-s3-operations-runbook.md) defines the deployment handoff and drills. These are implementation surfaces only: exporter wiring, retry visibility, credential-expiry detection, cost/retention alerts, SLO thresholds, and exercised staging procedures remain deployment gates |
| Release | Locked artifact provenance, security review, load/soak/fault evidence, staged canary, rollback, and post-deploy smoke pass | Open; the Standard scan `bb69ddae-798a-4387-bb87-f3e7acd496cb` at pushed head `227f81932b48fa1fe8b4999ed619add812a88e42` reports zero reportable findings in the 22 directly reviewed W25 surfaces, with the remaining repository inventory of 596 files explicitly deferred for follow-up. The AWS transport override, mutable workflow references, bounded S3 metrics, and account-binding preflight from baseline `89992ce3406f3f7586e5b072af488df4b565ea90` are remediated, but hosted OIDC/deployment evidence, repository-coverage follow-up, load/soak/fault/recovery drills, canary, rollback, and post-deploy smoke remain open. Latest hosted run `35601403560` at head `2159976` fails safely at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket` before AWS authentication; the existing test-role trust still allows only the SSO administrator, not GitHub OIDC, so an approved IAM trust/environment change is required. Bounded publication, listing, quota/TTL, and backing-file-aware staging cleanup remediations have landed |

## Deployment contract

The production configuration must contain only non-secret provider identity:

```json
{
  "kind": "aws-s3",
  "bucket": "<private-production-bucket>",
  "region": "<aws-region>",
  "prefix": "<owned-volume-prefix>",
  "durable": true
}
```

Credentials must come from the supported AWS workload chain, such as web
identity, ECS task credentials, or EC2 instance credentials. Do not put access
keys, session tokens, or profile exports in JSON, source control, images, CI
logs, or support bundles. The `r2` provider remains the explicit choice for
S3-compatible endpoints; the AWS provider rejects endpoint overrides so a
green test cannot be mistaken for AWS evidence.

The metadata provider, bucket, prefix, and workload identity must be reviewed
as one deployment. The S3 role must be limited to the required bucket/prefix
operations, while maintenance and cleanup authority must be separate and
audited. Decide explicitly whether versioning and SSE-KMS are enabled; if they
are enabled, the cleanup, restore, key-policy, and cost procedures must cover
object versions and KMS access rather than treating current-object deletion as
complete cleanup.

The live qualification harness also has an opt-in AWS S3 plus external PGlite
metadata row. On 2026-09-21, the scoped role passed the AWS CLI and block
acceptance tests, then `live_aws_s3_blocks_with_independent_pglite_metadata`
passed with a real PGlite socket server, a fresh metadata connection, a fresh
signed AWS client, filesystem reopen, and exact parent-prefix cleanup at
`mount-rs-tests/aws-s3/20260921T133321Z-23452-0493c0f8fe5454cbfd42f48dfd58f728/pglite`.
The expanded run at
`mount-rs-tests/aws-s3/20260921T134403Z-54972-b8831d9b39f99263ce764ba298b05302`
also passed independent-writer fencing, restored a temporary on-disk PGlite
data directory into a fresh server process, and reopened the same AWS-backed
filesystem. This is local metadata backup/restore and restart evidence only.
This is provider-pairing qualification only: the PGlite process is an
isolated test service, and production multi-writer fencing, independent
backup/restore, schema migration, failure recovery, and operational ownership
remain open.

## Reviewable infrastructure contract

[`infra/aws-s3-production.yaml`](../infra/aws-s3-production.yaml) is the
reviewable CloudFormation contract for one production bucket and one owned
mount-rs object prefix. It creates no IAM roles and grants no trust. The
deployment owner must supply and approve the existing short-lived runtime and
separate maintenance role ARNs, the production bucket name, the owned prefix,
the retention period, and the encryption choice.

The template defaults to BucketOwnerEnforced ownership, all four S3 public
access blocks, versioning enabled, retained state on stack deletion or
replacement, a one-day incomplete-multipart abort, and SSE-S3. It can select
SSE-KMS and an optional customer-managed key ARN. The runtime role can list,
read, and publish only objects below the owned prefix; version listing and
deletion are reserved for the maintenance role. A bucket policy denies
insecure transport.

The template was syntax-validated with the read-only AWS CloudFormation API on
2026-09-21; no stack or change set was created. Validation does not approve
the production parameters, role trust policies, metadata topology, backup
plan, or deployment promotion. Those remain W25.5-W25.9 gates.

## Minimum IAM policy shapes

The runtime role should be limited to the one volume prefix. Replace the
placeholders during an approved infrastructure change; do not paste live
values or credentials into this repository.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "VolumeObjects",
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:PutObject", "s3:AbortMultipartUpload"],
      "Resource": "arn:aws:s3:::<private-bucket>/<volume-prefix>/*"
    },
    {
      "Sid": "VolumeListing",
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::<private-bucket>",
      "Condition": {
        "StringLike": {
          "s3:prefix": ["<volume-prefix>", "<volume-prefix>/*"]
        }
      }
    }
  ]
}
```

The cleanup/maintenance role should be separately approved and audited. If
bucket versioning is enabled, its policy and runbook must explicitly cover
object-version listing and deletion; a successful current-object delete is not
proof that historical versions are gone. If SSE-KMS is selected, add only the
required `kms:Encrypt`, `kms:Decrypt`, and `kms:GenerateDataKey` permissions
on the named key and test the key policy with the runtime role.

For the protected GitHub OIDC workflow, the trust policy should restrict the
`token.actions.githubusercontent.com` subject to this repository and the
`aws-s3-ci` environment, require the `sts.amazonaws.com` audience, and grant
only the test bucket/prefix actions. The workflow intentionally references an
environment role rather than embedding a long-lived AWS secret; configure and
review that role before enabling hosted evidence. The workflow runs
`scripts/validate-aws-s3-ci-config.sh` before the credential action, so missing
or malformed protected inputs fail without making an AWS call. Set the
protected environment variable `MOUNT_RS_AWS_S3_ACCOUNT_ID` to the approved
12-digit account and require it to match the account component of
`MOUNT_RS_AWS_S3_CI_ROLE_ARN`; the preflight rejects a cross-account role ARN.

Run the read-only identity audit before changing the environment or role:

```sh
AWS_S3_CI_ROLE_ARN=<approved-ci-role-arn> \
./scripts/audit-aws-s3-ci-oidc.sh
```

The audit verifies the repository's immutable owner/repository subject shape,
the protected environment and main-branch policy, the required variable and
secret names, the AWS OIDC provider and `sts.amazonaws.com` audience, and an
exact `AssumeRoleWithWebIdentity` trust statement. It never reads secret
values or mutates GitHub or AWS. The current account audit is expected to fail
until the approved OIDC provider, role trust, protected environment, and CI
inputs are configured; that failure is a rollout blocker, not a hosted test
result.

## Rollout sequence

1. Review the resource and identity change, including region, bucket, prefix,
   encryption, retention, versioning, and metadata topology.
2. Run the locked provider, SDK, CLI, security, and acceptance gates with a
   unique canary prefix. Confirm that the canary role cannot read or delete a
   sibling prefix.
3. Run restart, concurrent-writer/fencing, backup/restore, failure-recovery,
   orphan-cleanup, and bounded load/soak drills against the intended topology.
4. Deploy one canary workload. Record the exact artifact, configuration
   digest, role identity, smoke result, metrics, and cleanup result.
5. Expand in stages only after the canary SLO window passes. Keep the prior
   artifact and metadata restore procedure available until the release owner
   signs off.
6. On rollback, stop new writers, preserve the metadata snapshot and S3
   prefix, restore the last known-good artifact/topology, and verify reads and
   ownership before resuming writes. Never bulk-delete a prefix as a rollback
   shortcut.

The authoritative checklist and evidence links live in the W25 section of
`WORK_TRACKER.md`. This document should be updated with deployment-specific
evidence when W25.5-W25.9 are completed; it must not be changed to imply a
production pass from test-account evidence alone.

## Read-only resource audit

Run the audit with an identity that is allowed to read bucket configuration,
not with the runtime prefix-scoped role:

```sh
AWS_PROFILE=<approved-audit-profile> \
AWS_S3_AUDIT_BUCKET=<private-bucket> \
AWS_S3_AUDIT_REGION=<aws-region> \
AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID=<approved-audit-account-id> \
./scripts/audit-aws-s3-resource.sh
```

The command fails closed on inherited AWS endpoint or service-profile overrides,
checks the caller account and bucket region, then checks all four Block Public
Access settings, BucketOwnerEnforced ownership, default server-side encryption,
the configured lifecycle expiry and multipart-abort days, and reports rather
than changes bucket versioning. It is safe to run during review, but a passing
qualification-bucket audit does not close the production-resource gate.

The latest current-tree qualification at pushed head `2633bec` passed this
audit in account `922978963556`, then passed the scoped sibling-prefix denial,
public Rust CLI write/shutdown/reopen/read, composed AWS S3 block, and
fresh-process reopen checks. Owned-prefix cleanup completed with
`AWS_S3_TEST_PASS` for
`mount-rs-tests/aws-s3/20260921T124037Z-88398-91d73f60bde3b905c3af2cc78b38b224`.
This is qualification-account evidence only; production resource, metadata,
identity, hosted release, and deployment operations gates remain open.

The live acceptance harness reads bucket versioning before it assumes the
prefix-scoped runtime role. Hosted jobs that use a separate audit identity must
provide the reviewed `None`, `Enabled`, or `Suspended` result as the protected
`AWS_S3_TEST_VERSIONING_STATUS` environment value; an absent or invalid value
blocks the run rather than weakening cleanup verification.
