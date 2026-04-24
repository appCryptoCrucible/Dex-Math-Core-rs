# Curve/Hydra Differential Report

- Iterations: `120000`
- Unit: `ns/op`
- Scope: curve-family adapter path differential + parity checks.

## Timing Breakdown

| Path | ns/op | ops/sec |
|---|---:|---:|
| ours_curve_total_quote | 11189.86 | 89366.60 |
| ours_curve_setup_rates_xp | 4979.65 | 200817.26 |
| ours_curve_kernel_dy | 3193.13 | 313172.34 |
| ours_curve_enrichment_prices_impact | 7959.06 | 125643.00 |
| curve_math_pool_get_amount_out | 3631.01 | 275405.47 |
| curve_math_stableswap_v2_kernel | 4549.83 | 219788.59 |
| hydra_hybrid_total_swap | 1033.67 | 967429.86 |
| hydra_hybrid_clone_only | 21.53 | 46446818.39 |
| hydra_hybrid_swap_core_estimate (total-clone) | 1012.14 | 988008.87 |

## Differential

- Ours vs curve-math total: `3.082x` slower.
- Ours vs hydra-hybrid total: `10.825x` slower.
- Ours setup+enrichment(exclusive) share estimate: `87.09%` of total.
- curve-math enum dispatch overhead estimate: `0.00%` (pool dispatch vs direct kernel).
- hydra clone overhead estimate in harness: `2.08%` of measured total.

## Accuracy/Parity

- Curve parity (ours vs curve-math StableSwapV2 mapping): exact matches `1000/1000 (100.00%)`, max abs diff `0` wei.
- Hydra parity (ours curve adapter vs hydra hybrid): exact matches `127/1000 (12.70%)`, max abs diff `403029` wei.

Note: parity numbers above are for this exact fixture and amp/fee mapping only; they are not universal proofs across all pool templates.
