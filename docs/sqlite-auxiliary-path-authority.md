# SQLite auxiliary path authority for concurrent mounts

SQLite creates rollback journals, WAL files, and shared-memory files beside a
database using the pathname it opens. Two names for one database inode can
select different auxiliary files, even when the database has only one hard
link. A Linux file bind mount is an example: both paths have the same device
and inode and `nlink == 1`, yet each path selects its own `-wal` and `-shm`.

For `MRC2`, mount-rs binds each local SQLite metadata database to its opened
canonical pathname when the fresh database is initialized, alongside its
physical device and inode. It binds each local SQLite block database to its
canonical pathname when claiming block authority. The pathname is stored as
lowercase hex of the raw Unix bytes, so its identity does not depend on a
lossy UTF-8 conversion. A symlink that resolves to the same canonical path
can use the authority. A hard link, file bind mount alias, copy, moved path,
or retargeted path is refused before bound publication or block verification.

Pathless `MRC2` prototype markers from development before release fail closed
on open. There is no automatic restamp: that marker cannot tell which
pathname had the authoritative journal/WAL namespace. Legacy and `MRC1`
metadata remain readable; their old unstamped copies cannot automatically
enroll in `MRC2`. The current migration command cannot repair a pathless
prototype marker. Such databases must be retired; a trusted offline recovery
procedure would need to be designed and audited before reusing them for
concurrent mounts.

This check does not prove SQLite's own file descriptor identity across an
adversarial path swap after open. Local macOS and qualified Linux filesystems
are still required for concurrent SQLite; NFS, network filesystems, Linux
overlayfs, and unknown Linux filesystem types are refused.

Coverage: the SQLite unit suite checks alternate valid path markers without
publication or authority mutation, old missing markers, symlink aliases,
non-UTF-8 pathname byte encoding, and hard links. The Linux file bind mount
regression is an ignored native test requiring a Linux mount namespace with
`CAP_SYS_ADMIN` and qualified local `/tmp` backing; it does not run on macOS.
