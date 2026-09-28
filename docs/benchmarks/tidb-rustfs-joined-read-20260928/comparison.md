# Joined TiDB read: complete matched remote10 comparison

One accepted baseline and one matched bounded control; no capacity, crash or physical IOPS claim.

| Mode | Pattern | Baseline cycles/s | Joined cycles/s | Ratio | Read/write RPC/s before → after |
| --- | --- | ---: | ---: | ---: | ---: |
| mostly_idle | sequential_read | 71.00 | 63.24 | 0.891 | 71.00 → 63.24 |
| mostly_idle | random_read | 79.42 | 64.68 | 0.814 | 79.42 → 64.68 |
| mostly_idle | sequential_overwrite | 39.49 | 35.65 | 0.903 | 39.49 → 35.65 |
| mostly_idle | random_overwrite | 40.48 | 34.83 | 0.860 | 40.48 → 34.83 |
| mostly_idle | mixed | 52.78 | 44.89 | 0.850 | 52.78 → 44.89 |
| mostly_idle | hot_file | 45.98 | 40.36 | 0.878 | 45.98 → 40.36 |
| mostly_idle | append_truncate | 48.18 | 37.22 | 0.773 | 24.58 → 18.61 |
| mostly_idle | churn | 16.76 | 14.81 | 0.883 | 0.00 → 0.00 |
| all_active | sequential_read | 363.85 | 325.26 | 0.894 | 363.85 → 325.26 |
| all_active | random_read | 386.99 | 297.95 | 0.770 | 386.99 → 297.95 |
| all_active | sequential_overwrite | 170.87 | 118.61 | 0.694 | 170.87 → 118.61 |
| all_active | random_overwrite | 174.91 | 120.90 | 0.691 | 174.91 → 120.90 |
| all_active | mixed | 236.69 | 213.77 | 0.903 | 236.69 → 213.77 |
| all_active | hot_file | 196.51 | 102.27 | 0.520 | 196.51 → 102.27 |
| all_active | append_truncate | 210.67 | 71.57 | 0.340 | 108.23 → 36.66 |
| all_active | churn | 91.91 | 56.50 | 0.615 | 0.00 → 0.00 |

Open/close, truncate and churn belong to metadata RPCs; all acknowledgements remain separate. Active clocks exclude metric observers and idle liveness. SQL/adapter calls are logical API observations, not physical IOPS or HTTP attempts.

JSON retains separate normalized API calls and inclusive wall times. Async spans overlap; frontend CPU includes observer/background work. Each run passed both fresh backend-context byte/membership/EOF/reopen phases and scope/revocation oracles; final bytes may differ with timed append cycles.

The candidate binary is bound to the accepted release build and applied provider source manifest. The baseline is73afa304; the candidate is clean committed083c8291, with verified source/progress identity. Exactly four source-file differences are disclosed: three joined-read files plus the accepted Windows-only fixture test correction. No CI or allocation proof is implied.
