# Compressed production-target metric evidence

## Observed problem

The current TiDB/RustFS D10 control completed all 16 workload patterns but its
owner stopped it before final verification. Retained native files reached
153,915,167 bytes, exceeding the 128 MiB sampled capture bound. Ordinary metric
frames contributed 121,004,195 bytes. The failed run remains unqualified.

## Design

Publish metric frames as individual gzip streams at explicitly named
`.json.gz` paths. Stream JSON into a fast gzip encoder, finish the stream before
the existing immutable hard-link publication, and retain every frame. Existing
acknowledgments and boundary records hash the encoded file. Decoding must recover
the identical JSON identity, counters, gauges and coverage fields.

Read compressed metric files with a single-member decoder, a 16 MiB encoded
bound and a 64 MiB decoded bound. Reject corruption, truncation, trailing bytes
and concatenated streams. Ordinary configuration, command and terminal JSON
receipts retain their existing representation.

Compression preserves replayable boundary evidence. Deleting consumed frames
would break existing path/digest references. Increasing the owner capture limit
would retain the observed amplification, so neither approach is selected.

## Verification

First reproduce the missing compression using the real publication and receipt
reader. Verify exact round trips, immutable publication, malformed streams and
decoded limits. Run release target tests, strict Clippy and formatting before
commit. Freeze a new Cargo-emitted executable/source inventory, then rerun the
unchanged strict D10 geometry and require both fresh oracles and complete cleanup.
Record encoded capture bytes and compression observer costs. D100/full geometry,
distributed-cache performance and remaining platform/CI/merge gates remain open.
