# Native backing comparison helpers

This package currently supplies input contracts and artifact readers for the
portable TiDB/filesystem versus TiDB/RustFS comparison work. It does not start
providers or implement a comparison runner.

`read_artifact` returns capped bytes and their SHA256 from the same descriptor.
`digest_artifact` hashes without retaining the whole file. Both require an
absolute path without parent traversal, reject symlinks, FIFOs and hardlinks,
and check the opened file and its directory entries before returning. An
optional expected hash must be explicit lowercase SHA256. Files must belong
to the invoking user. Unix descriptor APIs are required.

`strict_json`, `uint64`, `require_keys` and `parse_backend` supply closed input
primitives. Callers still validate their schema and field values. Imports have
no filesystem, process or network effects.

Run the controls on Linux or macOS:

```sh
python3 -I -S -B -m unittest discover -s scripts -p test_native_backing_artifacts.py
```

The tests exercise real descriptor reads, path replacement, inode changes,
FIFO refusal, closure on errors and bounded memory for streaming hashes.
These checks establish the reader contract. A runner must separately own its
processes and enforce deadlines, RSS, output and disk limits. Descriptor
continuity checks do not make a filesystem namespace atomic or guarantee
latency on every mounted filesystem.
