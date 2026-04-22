# Dex Math Core (Rust)

High-accuracy, deterministic DEX math extracted from a production MEV/liquidation system.

This crate is focused on **math and state-shape primitives** only. Strategy logic, routing
heuristics, execution pipelines, and private alpha components are intentionally out of scope.

## Scope

- Uniswap V2 constant-product math
- Uniswap V3 concentrated-liquidity math
- Curve StableSwap math and curve-math pool bridge
- Balancer weighted-pool math
- Kyber Elastic math
- Shared precision/error/domain types used by the modules above

## Design Goals

- Deterministic integer arithmetic for financial correctness
- Fast execution paths suitable for latency-sensitive systems
- Explicit error handling (no panic-based control flow)
- Minimal runtime coupling to external infrastructure

## Module Map

See `docs/MODULE_MAP.md`.

## Build

```bash
cargo check
```

## Contributing

Contributions are welcome, especially for:

- correctness fixes
- additional DEX math modules
- test vectors and parity tests

See `CONTRIBUTING.md` for contribution workflow and requirements.

## License

MIT. See `LICENSE`.

## Notes For Integrators

- `dex::adapter` now depends on `data::PoolStateProvider` (trait), not a concrete manager.
- Implement `PoolStateProvider` in your host app and keep state management external.
- This repository currently preserves original source structure to keep diff history clear.

## Security And Accuracy

- Numeric behavior is exact-integer where possible.
- If a computation cannot be performed safely, functions return structured errors.
- Consumers should treat errors as fail-closed signals, not opportunities for guessed fallbacks.
