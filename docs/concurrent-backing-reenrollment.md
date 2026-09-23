# Recover an old unstamped SQLite MRC1 volume

Ordinary `migrate-concurrent-backing` refuses SQLite metadata files that
predate physical device/inode stamps. An automatic stamp would trust any
copied database with the same logical volume ID. The explicit recovery command
is limited to an unstamped, fenced MRC1 metadata file; it does not restamp a
stamped MRC1 or MRC2 file or enroll a historical Legacy file.

Stop every writer using the volume, including writers on other hosts. Select
the one metadata file that will remain authoritative and remove other copies
from service before recovery. Keep all writers stopped until recovery finishes
and all clients use the upgraded binary. The tool cannot prove that every
writer is stopped or that an unknown metadata copy does not exist. The
assertion flag records the operator's trust in those conditions; a volume ID
alone does not prove uniqueness because a copied database retains it.

Use the exact split-store config with `storage.concurrent_writes: true` and
SQLite metadata. Both SQLite backing paths must be on supported local storage
outside every configured mountpoint. Before opening providers, the CLI rejects
mounted views, creates missing mountpoint directories, and rechecks backing
placement using their filesystem identity. Those directories can remain after
failure. Read the current revision and volume ID from the selected file:

```sh
sqlite3 /absolute/path/to/metadata.sqlite \
  'SELECT revision, volume_id FROM mount_rs_metadata WHERE id=1;'
mount-rs reenroll-sqlite-concurrent-backing \
  --config /absolute/path/to/shared.json \
  --expected-revision 7 \
  --expected-volume-id 'the-selected-volume-id' \
  --assert-all-writers-stopped-and-sole-metadata-copy
```

Replace `7` and the volume ID with the exact values read from that file. The
command validates the namespace, exact revision, volume ID, inactive version
state, and every referenced block before claiming block authority. It then
atomically stamps the selected physical metadata file's device, inode, and
canonical pathname and changes its mode to MRC2 with a conditional provider
update. The pathname uses the existing raw Unix byte encoding described in
[SQLite auxiliary path authority](sqlite-auxiliary-path-authority.md). The
metadata revision, namespace, and volume ID are preserved. It does not start
a native mount.

A wrong expectation, a missing or short referenced block, or an invalid
metadata stamp fails before a new block authority marker is claimed. The final
metadata update rechecks the preflight conditions. Because metadata and blocks
can be separate providers, a concurrent change or failure after the block claim
can leave an unused block marker. Keep the volume offline and investigate the
selected config before retrying. Start upgraded clients only after the command
reports success.
