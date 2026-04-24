# Competitor Delta vs Previous Report

- Previous comparison source: prior `benches/competitor_report.md` snapshot (ours `ns/op`: 1237.76, 2565.30, 4192.79, 1938.54, 11379.48, 528.82, 5034.41)
- Current comparison source: latest rerun of `src/bin/competitor_harness.rs` with `--iters 50000`
- Unit: `ns/op` (lower is faster)

## Ours: Previous vs Current

| Case | Ours (Prev) | Ours (Current) | Delta |
|---|---:|---:|---:|
| `uniswap_v2_vs_uniswap-v2-sdk` | 1237.76 | 509.54 | +58.84% |
| `uniswap_v3_vs_uniswap-v3-sdk` | 2565.30 | 2435.28 | +5.07% |
| `uniswap_v4_hookless_vs_uniswap-v4-sdk` | 4192.79 | 2736.55 | +34.73% |
| `balancer_weighted_vs_balancer-maths-rust` | 1938.54 | 757.11 | +60.95% |
| `curve_adapter_vs_curve-math` | 11379.48 | 4333.62 | +61.92% |
| `uniswap_v2_vs_hydra_constant_product` | 528.82 | 466.83 | +11.72% |
| `curve_adapter_vs_hydra_hybrid` | 5034.41 | 4711.13 | +6.42% |

## Relative Position (Current)

- Wins: `uniswap_v2_vs_uniswap-v2-sdk` (ours remains faster)
- Near parity: `uniswap_v4_hookless_vs_uniswap-v4-sdk` (ours slightly slower)
- Still behind: `uniswap_v3_vs_uniswap-v3-sdk`, Balancer canonical, Curve canonical, Hydra CP, Hydra hybrid
