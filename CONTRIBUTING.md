# Contributing

Thanks for contributing to `dex-math-core-rs`.

## What We Want Contributions For

- Math correctness fixes and edge-case handling
- New DEX math modules with tests
- Performance improvements that preserve exactness
- Better test vectors and parity tests against canonical implementations
- Documentation improvements

## Non-Negotiables

- Financial math must be deterministic and fail-closed.
- No guessed fallback outputs for failed calculations.
- Preserve integer-precision behavior; avoid floating point in core math paths.
- Include tests for all behavior changes.

## Setup

```bash
cargo check
cargo test
```

## Pull Request Checklist

- [ ] Tests added/updated for the change
- [ ] `cargo check` passes
- [ ] `cargo test` passes
- [ ] Any new module is wired into `src/dex/*/mod.rs` and documented in `docs/MODULE_MAP.md`
- [ ] Change notes explain correctness and rounding behavior

## Adding A New DEX Math Module

1. Add module files under `src/dex/<dex_name>/`.
2. Add `mod.rs` exports under `src/dex/<dex_name>/` and `src/dex/mod.rs`.
3. Add unit/property tests with edge-case coverage.
4. Add an entry to `docs/MODULE_MAP.md`.
5. Add integration notes to `README.md` if needed.

## Reporting Math Issues

Open an issue with:

- Exact function name and inputs
- Expected behavior with source/reference
- Actual behavior
- Minimal reproducible test case
