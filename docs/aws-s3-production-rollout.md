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
| AWS resource controls | Reviewable IaC or equivalent, private bucket, Block Public Access, Object Ownership, encryption/KMS decision, lifecycle/versioning decision, and prefix ownership | Read-only audit script added; test-resource controls passed; production resource review open |
| Identity | Runtime and maintenance roles are least-privilege, short-lived, trusted only by the intended workload, and have no committed access keys | Test role and sibling-prefix denial passed; production workload identity open |
| Metadata | A durable, independently operated metadata provider is selected and qualified for the intended host/multi-writer scope | Open; SQLite is single-host evidence only |
| Recovery | Backup/restore, schema migration, orphan-block cleanup, restart, failure recovery, and disaster-recovery drills pass | Open |
| Operations | S3 latency/error/retry/conditional-conflict signals, credential-expiry detection, cost/retention alerts, SLOs, and incident runbooks exist | Open; the S3 gateway now exposes a bounded `S3Session::stats()` snapshot for latency, buffered bytes, operation counts, and authentication/conditional/throttling/client/server error classes. The public SDK's optional observability path also records provider block latency, errors, and bytes. These are instrumentation surfaces only: exporter wiring, retry visibility, credential-expiry detection, cost/retention alerts, SLO thresholds, and an exercised incident runbook remain deployment gates |
| Release | Locked artifact provenance, security review, load/soak/fault evidence, staged canary, rollback, and post-deploy smoke pass | Open; the Standard scan `f73dd069-4102-4465-aaf7-8d6165282a36` at pushed head `b46e37fcd686273b5fa9ddf686e5e831f537a2ee` reports zero reportable findings in the 21 directly reviewed W25 surfaces, with the remaining repository inventory of 592 files explicitly deferred for follow-up. The AWS transport override and mutable workflow references from baseline `89992ce3406f3f7586e5b072af488df4b565ea90` are remediated, but hosted OIDC/deployment evidence, repository-coverage follow-up, load/soak/fault/recovery drills, canary, rollback, and post-deploy smoke remain open. Hosted run `35598843178` now fails safely at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket` before AWS authentication; the existing test-role trust still allows only the SSO administrator, not GitHub OIDC, so an approved IAM trust/environment change is required. Bounded publication, listing, quota/TTL, and backing-file-aware staging cleanup remediations have landed |

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
      "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
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
or malformed protected inputs fail without making an AWS call.

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

The live acceptance harness reads bucket versioning before it assumes the
prefix-scoped runtime role. Hosted jobs that use a separate audit identity must
provide the reviewed `None`, `Enabled`, or `Suspended` result as the protected
`AWS_S3_TEST_VERSIONING_STATUS` environment value; an absent or invalid value
blocks the run rather than weakening cleanup verification.
