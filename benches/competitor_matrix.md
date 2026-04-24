# Competitor Matrix

This matrix defines the direct, reproducible benchmark targets used by `src/bin/competitor_harness.rs`.

## Cases

| Exchange Family | Our Path | Competitor Path | Harness Case ID |
|---|---|---|---|
| Uniswap V2 | `dex::uniswap_v2::adapter_math::quote_exact_input` | `uniswap-v2-sdk` pair quote path | `uniswap_v2_vs_uniswap-v2-sdk` |
| Uniswap V3 | `dex::uniswap_v3::adapter_math::quote_exact_input` | `uniswap-v3-sdk` pool quote path | `uniswap_v3_vs_uniswap-v3-sdk` |
| Uniswap V4 (hookless canonical lane) | `dex::uniswap_v4::adapter_math::quote_exact_input` | `uniswap-v4-sdk` hookless pool quote path | `uniswap_v4_hookless_vs_uniswap-v4-sdk` |
| Balancer Weighted | `dex::balancer::adapter_math::quote_exact_input` | `balancer-maths-rust` weighted formula | `balancer_weighted_vs_balancer-maths-rust` |
| Curve StableSwap | `dex::curve::adapter_math::quote_exact_input` | `curve-math` pool `get_amount_out` | `curve_adapter_vs_curve-math` |
| Non-canonical V2-like | `dex::uniswap_v2::adapter_math::quote_exact_input` | `hydra-amm` constant-product pool | `uniswap_v2_vs_hydra_constant_product` |
| Non-canonical stableswap-like | `dex::curve::adapter_math::quote_exact_input` | `hydra-amm` hybrid pool | `curve_adapter_vs_hydra_hybrid` |

## Notes

- Kyber has no mature canonical Rust math crate suitable for apples-to-apples inclusion in this matrix.
- V4 competitor lane is intentionally hookless canonical path only; hook-aware custom logic remains benchmarked internally by `perf_harness`.
- Output unit is `ns/op` (nanoseconds per operation), with `ops/sec` and relative speedup.
