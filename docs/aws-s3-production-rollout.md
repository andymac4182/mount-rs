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
| Operations | S3 latency/error/retry/conditional-conflict signals, credential-expiry detection, cost/retention alerts, SLOs, and incident runbooks exist | Open; the S3 gateway exposes a bounded `S3Session::stats()` snapshot for latency, buffered and consumed streaming request/response bytes, operation counts, and authentication/conditional/throttling/client/server error classes. The immutable AWS/R2 block adapter now also exposes a bounded, clone-shared `R2BlockStore::stats()` snapshot for logical block operations, latency, bytes, conditional ID collisions, terminal retry-exhaustion markers, and bounded error classes. The public SDK's optional observability path records provider block latency, errors, bytes, and reconciliation scanned/protected/recent/deleted counts through local snapshots, tracing, and OTLP counters. [`docs/aws-s3-operations-runbook.md`](aws-s3-operations-runbook.md) defines the deployment handoff and drills. These are implementation surfaces only: exporter wiring, successful internal retry-attempt measurement, credential-expiry detection, cost/retention alerts, SLO thresholds, and exercised staging procedures remain deployment gates |
| Release | Locked artifact provenance, security review, load/soak/fault evidence, staged canary, rollback, and post-deploy smoke pass | Open; the sealed current-source Standard scan `02d2c6eb-66e1-41f8-be59-d14aab9fde87` targets `4ebba4926045de28e9f03ac75b938947f4487a4b`, reports zero reportable findings across six W25 surfaces, and records partial coverage of a 650-file inventory. Its deferred non-W25 surfaces and external AWS/GitHub deployment state are not a production approval. Hosted run [`35629600687`](https://github.com/andymac4182/mount-rs/actions/runs/35629600687) at `62383df` passed provenance and synthetic contract suites, then safely stopped at `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; AWS authentication and acceptance were skipped. OIDC/protected-environment setup, load/soak/fault/restore, canary, rollback, and post-deploy smoke remain open. Bounded publication, listing, quota/TTL, backing-file-aware staging cleanup, the explicit AWS client retry budget, and SDK diagnostic redaction have landed |

The sealed Standard scan is current source-review evidence for the six W25
surfaces named above, not a repository-wide or deployment acceptance result.
Its canonical coverage is partial: the scan closed six W25 review rows against
a 650-file inventory and explicitly deferred unrelated repository surfaces and
live AWS/GitHub deployment state. Those deferred controls remain release gates.

The latest sealed W25 Standard scan is `4ba52479-904a-41d2-82d2-9afc20a82931`
at pushed source `aa529c58ce6801c69d3e6cc0ed8bb5ac7a8d9cf8` (2026-09-22). It
reported zero reportable findings across the six W25 surfaces and recorded
partial coverage of the 659-file repository inventory. The independent baseline
and architecture reviewers did not return within the bounded review window and
were not counted as completed coverage; unrelated repository surfaces and live
AWS/GitHub state remain deferred. This is security review evidence, not a
production approval.

The hosted workflow also records the exact source SHA, root and standalone AWS
test lockfiles, template,
policy/preflight/CloudFormation/audit/acceptance harness hashes, Rust and Ruby
toolchain metadata, and
bounded acceptance log in a pinned
14-day artifact. The latest run [`35629600687`](https://github.com/andymac4182/mount-rs/actions/runs/35629600687)
at `62383df` passed root and standalone AWS manifest provenance capture and all
four synthetic contract suites, then safely refused the unconfigured protected
environment with `AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; its non-expired
artifact is `aws-s3-qualification-35629600687-1` (7,649 bytes). This artifact
is available for a completed run or a safe preflight refusal; it does not substitute for
successful AWS authentication, acceptance, or production deployment evidence.

The newer observed hosted run [`35635498647`](https://github.com/andymacclenaghan/mount-rs/actions/runs/35635498647)
at `24408f8` also stopped before AWS authentication at
`AWS_S3_CI_CONFIG_BLOCKED missing_bucket`; its protected bucket, region,
account, versioning, and role inputs were blank. This is a current safety
refusal rather than an implementation failure or AWS acceptance result.

The latest tested integrated repository boundary
`43a34d28c2173c2429bcfa1f653dd048dc150b22` passed formatting, the full locked
offline workspace/all-target test gate with the required local loopback
permission, and strict workspace Clippy with `-D warnings` on the explicitly
isolated Cargo target `/private/tmp/mount-rs-w25-current-stats-gate`. The gate
included the 9P loopback integration, the 16-case R2 provider suite including
the bounded block-store diagnostics test, the 18-case S3 gateway suite, and
the other non-ignored workspace rows. The isolated target was used because
concurrent worktrees share the normal Cargo target and can expose cross-
worktree artifact races; this gate therefore binds to the checked-out source
rather than another thread's compiled metadata. Ignored native/service rows
remain explicit prerequisites and are not treated as production acceptance.

The current shared `origin/main` boundary at
`31e122bc2e5790bb3568c01aaea4b236d89dce96` passed on 2026-09-22 after the
security-evidence rebase: formatting, the full locked offline workspace/all-
target test gate with the required local loopback permission, and strict
workspace Clippy with `-D warnings` on the isolated Cargo target
`/private/tmp/mount-rs-w25-current-main-gate`. The credential-free template,
bucket-policy, CI-config, and CI-environment contract fixtures also passed.
Explicitly ignored native/service rows and all production deployment gates
remain separate prerequisites.

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
The pushed revision `56ef9ab` then passed a fresh scoped packet under
`mount-rs-tests/aws-s3/20260921T165854Z-84404-217dc2bf24fb46f9e3b96e88ba4fd4a4`:
sibling-prefix denial, the public SDK/CLI self-test, composed filesystem,
process reopen, independent PGlite metadata, writer fencing, PGlite
backup/restore and fresh-server reopen, and exact cleanup all passed. The
standalone `tests/aws/Cargo.lock` was refreshed for the current
`mount-rs-fuse` `futures-util` dependency, restoring the harness's `--locked`
reproducibility. This remains qualification-account and local-metadata
evidence only. A fresh current-source scoped rerun at pushed source
`860492d8595b665361e8d9eff46498280fc8de1f` on 2026-09-22 passed the same
dedicated-role sibling-prefix denial, public SDK/CLI self-test, composed AWS
S3 filesystem, process reopen, independent-PGlite metadata, writer fencing,
PGlite backup/restore, fresh-server reopen, and exact cleanup gates under
`mount-rs-tests/aws-s3/20260921T173952Z-67696-542c6ba2bf6552e46bc85e0c0873bf8c`.
Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted. This is
refreshed qualification-account and local-metadata evidence only; production
metadata ownership, independent backup/restore, schema migration, failure
recovery, DR, and operational sign-off remain open.
- The current shared source `2101e5553e2594ce6e24aac6e28510cce2ec0b96` passed
  a fresh authorized `myroot` qualification on 2026-09-22 under
  `mount-rs-tests/aws-s3/20260921T180316Z-14071-597ad4c6c87b5c310fe02d98120c0036`:
  sibling-prefix denial, public SDK/CLI self-test, composed AWS S3 filesystem,
  process reopen, independent PGlite metadata, writer fencing, PGlite
  backup/restore, fresh-server reopen, and exact owned-prefix cleanup all
  passed. Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted.
  This remains qualification-account and local-metadata evidence only, not
  production deployment acceptance.
- The latest pushed source `870348184b5a047faea01f584a68cb961b34f810` passed a
  fresh authorized `myroot` qualification on 2026-09-22 under
  `mount-rs-tests/aws-s3/20260921T183403Z-86652-1728a866a9affcd4348775aa8416f075`:
  sibling-prefix denial, public SDK/CLI self-test, composed AWS S3 filesystem,
  process reopen, independent PGlite metadata, writer fencing, PGlite
  backup/restore, fresh-server reopen, and exact owned-prefix cleanup all
  passed. Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted.
  This remains qualification-account and local-metadata evidence only, not
  production deployment acceptance.
The current shared-mainline qualification at pushed source
`0d017f09453af530517a2dfef5dc251c1a827932` passed on 2026-09-22 under `myroot`
and the dedicated test role. The scoped packet passed sibling-prefix denial,
public SDK/CLI self-test, composed AWS S3 filesystem, process reopen,
independent PGlite metadata, writer fencing, PGlite backup/restore,
fresh-server reopen, and exact owned-prefix cleanup under
`mount-rs-tests/aws-s3/20260921T190207Z-39577-3e389412508fea6c7c806b9477ffaf8f`.
Both `AWS_S3_TEST_PASS` and `AWS_S3_PGLITE_TEST_PASS` were emitted. This is
current qualification-account and local-metadata evidence only; production
resource, metadata, DR, hosted release, and operational gates remain open.
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
At pushed source `70d37fe`, the credential-free template contract, synthetic
bucket-policy contract/tamper cases, seven-case CI-input validator, and
three-case protected-environment fixture all passed locally. These safeguards
prove fail-closed validation only; they do not approve external GitHub or AWS
deployment state.
The same four fixtures passed again at current pushed source
`f950e5b87092504cc43a8f68a7fcc07098abc345` on 2026-09-22:
`AWS_S3_TEMPLATE_CONTRACT_PASS`, `AWS_S3_BUCKET_POLICY_TEST_PASS cases=2`,
`AWS_S3_CI_CONFIG_TEST_PASS cases=7`, and
`AWS_S3_CI_ENVIRONMENT_TEST_PASS cases=3`. This is still fail-closed local
validation only; it does not approve external GitHub or AWS resources.
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
result. The fresh read-only audit at pushed source `56ef9ab` on 2026-09-22 returned
`AWS_S3_OIDC_AUDIT_BLOCKED` for the missing environment protection rules,
non-self-approvable reviewer, protected-environment inputs and secret, missing
GitHub OIDC provider, and missing immutable-subject role trust; it made no
changes. A current-source rerun at pushed source
`cf18d93d7fdd1656d853f208db81b8b133265fe5` returned the same blocked set and
made no changes. A current read-only rerun at pushed source
`ce7b365a45f718009f557035d2549fd0faf2c8a8` through the authenticated `myroot`
profile on 2026-09-22 returned the same fail-closed blocker set:
`environment_missing_protection_rule`,
`environment_missing_protected_branch_policy`,
`environment_missing_non_self_review_required_reviewer`, the four missing
protected environment inputs/secret, `missing_github_oidc_provider`, and
`role_missing_immutable_github_subject_trust`. It made no GitHub or AWS
changes; hosted OIDC evidence remains blocked until the deployment owner
configures and approves those controls.
The latest read-only rerun at pushed source
`9b5acfbac3d88d5f17a969defd447f5d44ee3023` on 2026-09-22 returned the same
fail-closed blocker set and made no GitHub or AWS changes; hosted OIDC evidence
remains blocked until the deployment owner configures and approves those
controls.

The current shared-mainline read-only OIDC audit at pushed source
`a03edef8bbdbebb20626b5fd7e62267f34ebb720` on 2026-09-22 returned
`AWS_S3_OIDC_AUDIT_BLOCKED` for the missing environment protection rule,
protected-branch policy, non-self-approvable reviewer, four protected
environment inputs/secret, GitHub OIDC provider, and immutable-subject role
trust. It made no GitHub or AWS changes; hosted OIDC evidence remains blocked
until the deployment owner configures and approves those controls.

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

The latest read-only qualification-bucket audit at pushed source `56ef9ab`
passed in account `922978963556` with the expected versioning status `None`,
alongside the existing public-access, ownership, encryption, lifecycle, and
multipart-abort checks. A current-source audit at pushed source
`313fb2f2a6bd6e68305a86bc55036d77c563ca8c` repeated those controls on
2026-09-22 without mutating AWS or rerunning the full service qualification.
A current shared-source audit at pushed source
`6e19c4d2388aac02c0a3278a3f524febdc4b07ec` on 2026-09-22 repeated the
qualification bucket's account/region binding, all four public-access blocks,
BucketOwnerEnforced ownership, AES256 encryption, `None` versioning, seven-day
`mount-rs-tests/` lifecycle, and one-day incomplete-multipart abort checks. It
made no AWS changes and remains qualification-account evidence only; the
production bucket, policy, roles, and approved change set remain open.
The latest read-only audit at pushed source
`9b5acfbac3d88d5f17a969defd447f5d44ee3023` on 2026-09-22 repeated the same
account/region, public-access, ownership, AES256 encryption, `None` versioning,
seven-day lifecycle, and one-day incomplete-multipart abort controls without
mutating AWS. It remains qualification-account evidence only; the production
bucket, policy, roles, and approved change set remain open.
The current shared-mainline read-only qualification-bucket audit at pushed
source `a03edef8bbdbebb20626b5fd7e62267f34ebb720` on 2026-09-22 passed the
same account/region, public-access, ownership, AES256 encryption, `None`
versioning, seven-day lifecycle, and one-day incomplete-multipart abort checks.
It made no AWS changes; this remains qualification-account evidence only and
the production bucket, policy, roles, and approved change set remain open.
The latest full integrated qualification is the current shared-source
`2101e555` run recorded above. This is qualification-account evidence only;
production
resource, metadata, identity, hosted release, and deployment operations gates
remain open.

The live acceptance harness reads bucket versioning before it assumes the
prefix-scoped runtime role. Hosted jobs that use a separate audit identity must
provide the reviewed `None`, `Enabled`, or `Suspended` result as the protected
`AWS_S3_TEST_VERSIONING_STATUS` environment value; an absent or invalid value
blocks the run rather than weakening cleanup verification.
