# TiDB structural publication lock scope

## Evidence and hypothesis

The indexed structural publisher locks the exact volume authority row before
reading the complete membership, selected guards, and complete directory entries.
The directory query also locks every returned entry, although targeted create,
rename, and unlink change only a few entries. At 1,000 root files this acquires
roughly 1,000 sibling row locks for five or fewer logical row changes.

TiDB's transaction implementation can carry unchanged pessimistically locked
keys through its commit mutation set. Measure the actual transaction write-key
histogram on the existing owned TiDB before changing the publisher. A slow COMMIT
in the earlier ten-server benchmark alone does not establish this cause.

## Candidate change

Remove only the directory-entry `FOR UPDATE` suffix inside
`compact_publish_structure`. Keep the exact authority lock, current guard locks,
Read Committed transaction mode, complete ordered membership and entry reads,
decoding, structural validation, preflight, publication DML, and ambiguous-COMMIT
handling. All cooperative compact membership/entry writers already acquire the
same authority lock. Selected inode writers can change only regular-file guards.

This does not reduce complete-read complexity or remove per-volume structural
serialization. Direct administrative SQL that bypasses provider ownership is not
a cooperative writer; preexisting corruption must still be rejected from the
complete fresh rows. No claim is made of fencing arbitrary concurrent raw SQL.

## Gates

- Establish baseline write keys for one create with four and 1,000 siblings.
  Qualify shared metrics only when the observed general transaction count is
  exactly one, series are stable, and a fresh complete oracle passes.
- Require a bounded write-key footprint independent of sibling count; watch the
  actual-TiDB regression fail before changing production code.
- Verify Read Committed publication rejects sibling corruption committed after
  transaction start, and overlapping unrelated selected writes are preserved.
- Preserve complete sibling/member corruption controls, guard-wait freshness,
  rollback, packet rejection, and lost-COMMIT-ack behavior.
- Run touched formatting, provider tests, strict Clippy, and independent review.
- Repeat the unchanged ten-server/ten-drive/1,000-file workload against a fresh
  source-bound release binary. Report actual rates and remaining qualification
  limits; do not change benchmark floors or requested durations.
