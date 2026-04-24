# Competitor Benchmark Report

- Iterations: `50000` (per primary case)
- Method: release-mode microbench with warmup and deterministic fixtures.
- Policy: fail-closed paths only; no synthetic fallback quotes.

| Case | Ours ns/op | Competitor ns/op | Ours ops/s | Competitor ops/s | Speedup (x) | Faster (%) |
|---|---:|---:|---:|---:|---:|---:|
| uniswap_v2_vs_uniswap-v2-sdk | 509.54 | 6549.40 | 1962546.76 | 152685.70 | 12.854 | 92.22 |
| uniswap_v3_vs_uniswap-v3-sdk | 2435.28 | 2029.38 | 410631.07 | 492761.34 | 0.833 | -20.00 |
| uniswap_v4_hookless_vs_uniswap-v4-sdk | 2736.55 | 2668.67 | 365423.88 | 374718.77 | 0.975 | -2.54 |
| balancer_weighted_vs_balancer-maths-rust | 757.11 | 288.92 | 1320815.52 | 3461117.80 | 0.382 | -162.04 |
| curve_adapter_vs_curve-math | 4333.62 | 1476.32 | 230753.75 | 677359.92 | 0.341 | -193.54 |
| uniswap_v2_vs_hydra_constant_product | 466.83 | 49.73 | 2142116.58 | 20106969.08 | 0.107 | -838.65 |
| curve_adapter_vs_hydra_hybrid | 4711.13 | 450.06 | 212263.39 | 2221906.22 | 0.096 | -946.77 |
