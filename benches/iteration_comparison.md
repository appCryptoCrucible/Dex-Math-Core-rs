# Iteration Performance Comparison

- Harness: `src/bin/perf_harness.rs`
- Iterations: `250000` (`curve_basic` uses `50000` as defined by harness)
- Unit: `ns/op` (lower is faster)

## Commits Compared

- Original reference: `74e3ad1`
- Last iteration: `bda8546`
- Current iteration: working tree after latest performance upgrades

## Results

| Case | Original (`74e3ad1`) | Last (`bda8546`) | Current (working tree) | Last -> Current | Original -> Current |
|---|---:|---:|---:|---:|---:|
| `v3_cross` | 7789.87 | 3148.18 | 3268.51 | -3.82% | +58.04% |
| `kyber_cross` | 3829.44 | 3198.51 | 3780.55 | -18.20% | +1.28% |
| `v4_cross` | 2476.35 | 2590.22 | 2527.63 | +2.42% | -2.07% |
| `curve_basic` | 7932.68 | 4982.16 | 4491.78 | +9.84% | +43.38% |
| `balancer_basic` | 814.81 | 828.75 | 663.61 | +19.93% | +18.56% |

## Notes

- `Last -> Current` is mixed: strong wins on `curve_basic` and `balancer_basic`, slight gain on `v4_cross`, regressions on `v3_cross` and `kyber_cross`.
- `Original -> Current` remains strongly improved overall, especially for Curve and V3.
