# Exact TiDB membership comparison

Structural writes retain fresh complete membership equality. A generation-only
cached proof would violate that contract. Compare the exact existing set inside
TiDB instead, preserving full enumeration for cases without a cheap exact proof.

Under the existing structural authority lock and Read Committed transaction,
compare every fresh authority field to the sealed delta's base. For scoped
create/rename/unlink, eligible expected IDs must be positive, contiguous in
strictly increasing order and fit signed SQL BIGINT. One constant statement
returns total stored rows and rows whose inode lies in the closed expected
interval. The validated `(volume_key,inode)` primary key makes IDs unique; the
interval contains exactly N expected IDs. Both counts equal N if and only if
the complete actual set equals those expected IDs.

Only successful fresh equality permits borrowing the immutable sealed base
anchor for the existing delta validator. Gaps, authority/count mismatch, invalid
bounds or insufficient packet budget use existing actual enumeration and retain
its error classification. Full snapshots and Full structural scope enumerate
actual IDs. Complete parent-entry reads, affected guard locks, preflighted DML
and COMMIT-before-ACK remain.

The proof binds just two integer bounds and the volume key, regardless of the
member count. Check conservative prepare/execute packet sizes against the same
client/session budget used for publication preflight. No proof, time, resource,
byte or workload gate is weakened. TiDB still scans O(F) members and returns O(F)
parent entries; this is neither allocation-free nor O(1) datastore work.

An earlier power-of-two IN-list candidate passed correctness but increased
F1000 median publication latency by 8.9% in eight paired trials. It reduced
allocation calls by 27%, yet only 1.2% of requested allocation bytes. Its large
bound parameter set increased inclusive inode-read time. That implementation
was rejected; its raw benchmark evidence is retained separately. The constant
range proof applies when membership remains contiguous. Create-only workloads
and retained root-file tombstones can preserve this layout; Full removals and
later allocation can introduce gaps. Sparse layouts retain enumeration.

Actual controls must check dense and sparse equal-count substitutions, fresh
parent proof after waits, packet refusals and unknown COMMIT outcomes. Paired
measurement must validate every intermediate created body through a fresh full
read before reset, then compare the final complete namespace through a new
context after the writer closes.
