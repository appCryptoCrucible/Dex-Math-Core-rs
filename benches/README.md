# Performance Benchmarks

This repository includes a deterministic quote-performance harness at `src/bin/perf_harness.rs`.

It benchmarks release-mode quote latency for:

- Uniswap V3 crossing path (`v3_cross`)
- Kyber Elastic crossing path (`kyber_cross`)
- Uniswap V4 crossing path (`v4_cross`)
- Curve stableswap quote path (`curve_basic`)
- Balancer weighted quote path (`balancer_basic`)

## Run Locally

```bash
cargo run --release --bin perf_harness -- --iters 400000
```

Notes:

- Units in output are **nanoseconds per iteration** (`ns_per_iter`).
- Curve uses `(iters / 5)` internally because each quote is materially heavier.
- For clean comparisons, run benchmarks sequentially and avoid concurrent Cargo jobs.

## Measured A/B Results

Comparison target:

- Baseline: commit `74e3ad1` (pre-optimization)
- Optimized: current optimized tree after quote hot-path performance work

See `benches/results-2026-04-23.csv` for raw numbers.

High-level outcome from that run:

- `v3_cross`: **56.88% faster** (2.32x)
- `kyber_cross`: **5.61% faster** (1.06x)
- `v4_cross`: **22.12% faster** (1.28x)
- `curve_basic`: **74.73% faster** (3.96x)
- `balancer_basic`: **73.07% faster** (3.71x)
- Synthetic aggregate mix: **45.45% faster** (1.83x)

## Accuracy and Safety Constraints

All optimizations retained fail-closed behavior and exact integer math semantics:

- no floating-point substitutions
- no unchecked arithmetic substitutions in critical paths
- no fallback guessing logic added
