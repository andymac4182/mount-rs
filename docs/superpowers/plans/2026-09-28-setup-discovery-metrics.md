# QUIC setup and cache discovery metrics

## Contract

Append `client.quic.connection_setup` and `blob_cache.discovery.locate` after the existing 108 storage rows. Preserve all prior names/ordinals and the 136 core rows. Use opt-in fixed-label spans with terminal success/error/cancellation, in-flight gauges and the existing latency histogram and bounded secret-free slow logs.

QUIC setup covers config/endpoint construction through actual connection and negotiated ALPN validation, excluding credentials, ClientHello and WebSocket fallback. A failed Auto QUIC attempt remains an error even if fallback succeeds. Discovery covers only the existing locate await inside the unchanged peer deadline, before filtering/hedging. Empty or fallback peer lists are successful lookups, not directory health evidence. Bytes and SQL returned rows are unavailable at both boundaries.

## Steps

- Add isolated behavior tests and retain semantic RED on missing producers.
- Instrument only existing boundaries, with no new tasks, boxing, deadlines, retries or protocol change.
- Append producer/exporter inventories and independent current fixtures; historical absence remains unavailable. Native addon source coverage remains unchanged.
- Run exact QUIC and cache controls, warmed zero-added-allocation recorder checks, consumer tests, affected Rust tests, strict Clippy and formatting.
- Review source and retained receipts independently; publish tested changes through PR #33.

## Limits

Inclusive wall time overlaps nested work and is not exclusive CPU/network time. Recorder allocation checks exclude setup, async/payload allocations, initialization, snapshots and logging. These local controls do not qualify production capacity, physical IOPS, directory maintenance, incoming peer setup or crash/power-loss durability.
