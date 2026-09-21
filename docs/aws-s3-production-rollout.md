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
| Release | Locked artifact provenance, security review, load/soak/fault evidence, staged canary, rollback, and post-deploy smoke pass | Open; the sealed Standard scan `bb69ddae-798a-4387-bb87-f3e7acd496cb` covers pushed head `227f81932b48fa1fe8b4999ed619add812a88e42` with zero reportable findings in 22 directly reviewed W25 surfaces, while the remaining repository inventory was explicitly deferred. The completed scan `c6992ddb-3762-4638-b37e-f1399bd77e42` at target `8e271cd24de1458519c9dca61363c427784a3dd3` found one medium `StoreConfig` debug-credential disclosure and partial coverage (10 W25 surfaces of a 606-file inventory). That finding is fixed at `3fca80270070635ea6e63c00099ab7342a2ffdf1` with a redacting SDK `Debug` implementation and regression test; the W25 evidence boundary recorded here is `20baa9e` after later IaC, CI, and documentation commits, so the scan predates the reviewed boundary and is not current-head security evidence. Hosted OIDC/deployment evidence, repository-coverage follow-up, load/soak/fault/recovery drills, canary, rollback, and post-deploy smoke remain open. The latest observed hosted run [`35625592317`](https://github.com/andymac4182/mount-rs/actions/runs/35625592317) at `fb34d8b` passed provenance capture and all four synthetic contract suites, then failed safely at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket` before AWS authentication; AWS identity and acceptance were skipped. The existing test-role trust still allows only the SSO administrator, not GitHub OIDC, so an approved IAM trust/environment change is required. Bounded publication, listing, quota/TTL, backing-file-aware staging cleanup, the explicit AWS client retry budget, and SDK diagnostic redaction have landed |

The hosted workflow also records the exact source SHA, lockfile/template,
policy/preflight/CloudFormation/audit/acceptance harness hashes, Rust and Ruby
toolchain metadata, and
bounded acceptance log in a pinned
14-day artifact. The latest run [`35625592317`](https://github.com/andymac4182/mount-rs/actions/runs/35625592317)
at `fb34d8b` passed provenance capture and all four synthetic contract suites,
then safely refused the unconfigured protected environment with
`AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; its non-expired artifact is
`aws-s3-qualification-35625592317-1` (6,836 bytes). This artifact is available for a
completed run or a safe preflight refusal; it does not substitute for
successful AWS authentication, acceptance, or production deployment evidence.

The latest tested integrated repository boundary `00d2b80277991a6d826874ad7e3e3c1593e34ae3`
passed formatting, the full locked offline workspace test gate, and strict
workspace Clippy with `-D warnings` on an explicitly isolated Cargo target
after rebasing onto `origin/main`, with the required local loopback permission.
The isolated target was used because concurrent worktrees share the normal
Cargo target and can expose cross-worktree artifact races; this gate therefore
binds to the checked-out source rather than another thread's compiled
metadata. Ignored native/service rows remain explicit prerequisites and are
not treated as production acceptance.

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

The AWS provider uses an explicit bounded object-store retry policy of five
retries and a 30-second retry window. This is a request-latency safety bound,
not an approved workload SLO or proof that internal retry attempts are
exported; the selected deployment still needs fault-injection measurement and
an owner-approved retry/error budget.

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
filesystem. The rerun after the explicit AWS S3 five-retry/30-second request
budget passed the CLI, composed, process-reopen, independent-PGlite,
fencing, and restore/reopen gates under
`mount-rs-tests/aws-s3/20260921T141112Z-81269-ab3a599172244316234d1f3b23181dba`.
This is local metadata backup/restore and restart evidence only.
A fresh current-source rerun at `3fca802` passed the same CLI, composed,
process-reopen, independent-PGlite, fencing, restore/reopen, and exact cleanup
gates under
`mount-rs-tests/aws-s3/20260921T144535Z-15427-5ae13eaf019b31185a11d784fdfdcf52`.
The latest integrated qualification at audit commit `d7ccdc7` passed the same
scoped-role, public SDK/CLI, composed filesystem, process-reopen, independent
PGlite, fencing, restore/reopen, and exact cleanup gates under
`mount-rs-tests/aws-s3/20260921T152108Z-66596-90bba238142882ef153e6e3c246d0003`;
the same source's read-only resource audit passed the account/region,
public-access, ownership, encryption, versioning, lifecycle, and multipart-
abort checks.
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
replacement, a one-day incomplete-multipart abort, and SSE-S3. Its parameter
rules require runtime and maintenance to be different roles, require a key ARN
when SSE-KMS is selected, and reject an unused key ARN for SSE-S3. Its bucket
policy denies insecure transport for the bucket and every object key. It can
select SSE-KMS and an optional customer-managed key ARN. The reviewed retention
period applies to current objects and noncurrent versions under the owned
prefix. The runtime role can list,
read, and publish only objects below the owned prefix; version listing and
deletion are reserved for the maintenance role.

The template was syntax-validated with the read-only AWS CloudFormation API on
2026-09-21 and revalidated after tightening `OwnedPrefix` to reject empty and
dot components, widening the transport deny to all object keys, expiring
noncurrent versions, and adding parameter rules for role separation and KMS
selection on 2026-09-22; no stack or change set was created.
The template bucket-name constraint and the read-only resource auditor now
reject consecutive dots and invalid length or edge characters consistently
with the hosted preflight.
Validation does not approve
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
or malformed protected inputs fail without making an AWS call. Its synthetic
seven-case regression matrix also rejects malformed bucket/region/prefix
shapes, static credentials, AWS profile/config overrides, and cross-account
role ARNs. Set the
protected environment variable `MOUNT_RS_AWS_S3_ACCOUNT_ID` to the approved
12-digit account and require it to match the account component of
`MOUNT_RS_AWS_S3_CI_ROLE_ARN`; the preflight rejects a cross-account role ARN.

Run the read-only identity audit before changing the environment or role:

```sh
AWS_S3_CI_ROLE_ARN=<approved-ci-role-arn> \
./scripts/audit-aws-s3-ci-oidc.sh
```

The audit verifies the repository's immutable owner/repository subject shape,
the protected environment and main-branch policy, a non-self-approvable
required reviewer, the required variable and secret names, the AWS OIDC
provider and `sts.amazonaws.com` audience, and one exact
`AssumeRoleWithWebIdentity` trust statement without additional broad GitHub
federation grants. It never reads secret values or mutates GitHub or AWS. The
current account audit is expected to fail
until the approved OIDC provider, role trust, protected environment, and CI
inputs are configured; that failure is a rollout blocker, not a hosted test
result. The fresh read-only audit at current source `32b0609` on 2026-09-22 returned
`AWS_S3_OIDC_AUDIT_BLOCKED` for the missing environment protection rules,
non-self-approvable reviewer, protected-environment inputs and secret, missing
GitHub OIDC provider, and missing immutable-subject role trust; it made no
changes.

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
AWS_S3_AUDIT_EXPECTED_VERSIONING_STATUS=Enabled \
./scripts/audit-aws-s3-resource.sh
```

The command fails closed on inherited AWS endpoint or service-profile overrides,
checks the caller account and bucket region, then checks all four Block Public
Access settings, BucketOwnerEnforced ownership, default server-side encryption,
the configured current-object lifecycle expiry, matching noncurrent-version
expiry when versioning is enabled, and multipart-abort days. When
`AWS_S3_AUDIT_EXPECTED_VERSIONING_STATUS` is set, it also fails closed unless
the live bucket matches the reviewed `None`, `Enabled`, or `Suspended` choice.
It reports rather than changes bucket versioning. It is safe to run during
review, but a passing qualification-bucket audit does not close the
production-resource gate.

For a production bucket, run the separate read-only policy audit with the
reviewed role bindings and owned prefix:

```sh
AWS_PROFILE=<approved-audit-profile> \
AWS_S3_AUDIT_BUCKET=<private-bucket> \
AWS_S3_AUDIT_REGION=<aws-region> \
AWS_S3_AUDIT_EXPECTED_ACCOUNT_ID=<approved-audit-account-id> \
AWS_S3_AUDIT_RUNTIME_ROLE_ARN=<approved-runtime-role-arn> \
AWS_S3_AUDIT_MAINTENANCE_ROLE_ARN=<approved-maintenance-role-arn> \
AWS_S3_AUDIT_OWNED_PREFIX=<owned-volume-prefix> \
./scripts/audit-aws-s3-bucket-policy.sh
```

It verifies the attached policy's full-bucket TLS deny and the exact
prefix-scoped runtime and maintenance statements from the reviewable
CloudFormation contract. It never prints the policy or role values and fails
closed when the bucket policy is absent or differs from that contract.

The latest read-only qualification-bucket audit at current source `32b0609`
passed in account `922978963556` with the expected versioning status `None`,
alongside the existing public-access, ownership, encryption, lifecycle, and
multipart-abort checks. It did not mutate the bucket or rerun the full service
qualification. The latest full integrated qualification remains the separate
`d7ccdc7` run recorded above. This is qualification-account evidence only;
production resource, metadata, identity, hosted release, and deployment
operations gates remain open.

The live acceptance harness reads bucket versioning before it assumes the
prefix-scoped runtime role. Hosted jobs that use a separate audit identity must
provide the reviewed `None`, `Enabled`, or `Suspended` result as the protected
`AWS_S3_TEST_VERSIONING_STATUS` environment value; an absent or invalid value
blocks the run rather than weakening cleanup verification.
