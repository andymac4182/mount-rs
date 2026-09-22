#ifndef MOUNT_RS_FSKIT_BRIDGE_H
#define MOUNT_RS_FSKIT_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#define MOUNT_RS_FSKIT_PROTOCOL_VERSION UINT16_C(1)
#define MOUNT_RS_FSKIT_MAX_BODY_LENGTH ((size_t)1024 * (size_t)1024)
#define MOUNT_RS_FSKIT_MAX_CONFIG_LENGTH ((size_t)64 * (size_t)1024)

enum mount_rs_fskit_dispatch_status {
    MOUNT_RS_FSKIT_STATUS_OK = 0,
    MOUNT_RS_FSKIT_STATUS_INVALID_ARGUMENT = -1,
    MOUNT_RS_FSKIT_STATUS_MALFORMED_REQUEST = -2,
    MOUNT_RS_FSKIT_STATUS_RESPONSE_TOO_SMALL = -3,
    MOUNT_RS_FSKIT_STATUS_INTERNAL_ERROR = -4,
};

uint16_t mount_rs_fskit_protocol_version(void);

/*
 * Dispatch exactly one bounded request frame.
 *
 * The function never allocates for the caller. response_len is required and
 * receives the encoded response length on success, or the required capacity
 * when the response buffer is too small. A nonzero request_len or
 * response_capacity requires the corresponding pointer to be non-null.
 *
 * A successful dispatch includes protocol-level error replies (for example,
 * protocol-only Operation message); malformed input is reported by the
 * negative status and has no fabricated response frame.
 */
int32_t mount_rs_fskit_dispatch(
    const uint8_t *request_ptr,
    size_t request_len,
    uint8_t *response_ptr,
    size_t response_capacity,
    size_t *response_len);

/*
 * Create a provider-selected worker from bounded UTF-8 JSON. The object must
 * contain "backend": "memory", "sqlite", or "splitSqlite". SQLite uses
 * "databasePath"; splitSqlite uses distinct "metadataPath" and "blocksPath"
 * values and optionally "chunkSize". "readOnly": true is a persistent
 * write-policy floor. A null return means invalid configuration or provider
 * initialization failure; no partially initialized worker is returned.
 */
void *mount_rs_fskit_create_worker(
    const uint8_t *config_ptr,
    size_t config_len);

/* Compatibility constructor for the deterministic in-memory backend. */
void *mount_rs_fskit_create_memory_worker(uint8_t read_only);

/* Dispatch through a persistent Rust FsDriver worker and its handle table. */
int32_t mount_rs_fskit_worker_dispatch(
    void *worker,
    const uint8_t *request_ptr,
    size_t request_len,
    uint8_t *response_ptr,
    size_t response_capacity,
    size_t *response_len);

/* Destroy a worker returned by either worker constructor. */
void mount_rs_fskit_destroy_worker(void *worker);

#endif
