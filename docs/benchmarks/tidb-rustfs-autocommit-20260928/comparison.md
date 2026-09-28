# Guarded autocommit TiDB read: complete matched remote10 comparison

One accepted baseline and one matched bounded control; no capacity, crash or physical IOPS claim.

| Mode | Pattern | Joined RR cycles/s | Autocommit cycles/s | Ratio | Read/write RPC/s before → after |
| --- | --- | ---: | ---: | ---: | ---: |
| mostly_idle | sequential_read | 63.24 | 124.24 | 1.965 | 63.24 → 124.24 |
| mostly_idle | random_read | 64.68 | 140.59 | 2.174 | 64.68 → 140.59 |
| mostly_idle | sequential_overwrite | 35.65 | 49.11 | 1.378 | 35.65 → 49.11 |
| mostly_idle | random_overwrite | 34.83 | 47.06 | 1.351 | 34.83 → 47.06 |
| mostly_idle | mixed | 44.89 | 73.57 | 1.639 | 44.89 → 73.57 |
| mostly_idle | hot_file | 40.36 | 57.88 | 1.434 | 40.36 → 57.88 |
| mostly_idle | append_truncate | 37.22 | 62.22 | 1.672 | 18.61 → 31.60 |
| mostly_idle | churn | 14.81 | 17.94 | 1.211 | 0.00 → 0.00 |
| all_active | sequential_read | 325.26 | 736.68 | 2.265 | 325.26 → 736.68 |
| all_active | random_read | 297.95 | 818.83 | 2.748 | 297.95 → 818.83 |
| all_active | sequential_overwrite | 118.61 | 201.03 | 1.695 | 118.61 → 201.03 |
| all_active | random_overwrite | 120.90 | 194.06 | 1.605 | 120.90 → 194.06 |
| all_active | mixed | 213.77 | 331.26 | 1.550 | 213.77 → 331.26 |
| all_active | hot_file | 102.27 | 238.33 | 2.330 | 102.27 → 238.33 |
| all_active | append_truncate | 71.57 | 258.63 | 3.613 | 36.66 → 132.21 |
| all_active | churn | 56.50 | 104.39 | 1.848 | 0.00 → 0.00 |

Open/close, truncate and churn belong to metadata RPCs; all acknowledgements remain separate. Active clocks exclude metric observers and idle liveness. SQL/adapter calls are logical API observations, not physical IOPS or HTTP attempts.

JSON retains separate normalized API calls and inclusive wall times. Async spans overlap; frontend CPU includes observer/background work. Each run passed both fresh backend-context byte/membership/EOF/reopen phases and scope/revocation oracles; final bytes may differ with timed append cycles.

The candidate binary is bound to the accepted release build and applied provider source manifest. The baseline is clean083c8291 (joined SELECT with explicit RR transaction); the candidate is clean52c1cdee (guarded autocommit SELECT), with verified build/source/progress identity. Seven compiled-source/test-file differences are pinned: four TiDB implementation/test files and three Windows test files. Five added published report documents are disclosed separately by the pinned current source review. No CI or allocation proof is implied.
