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

## Adapter API Map

The crate exposes decoupled `adapter_math` modules for deterministic quoting without runtime plumbing.
All adapters are fail-closed: invalid or incomplete state returns structured errors.

### Shared Pattern

- **Input**: `*PoolSnapshot` + swap params (`amount_in`, direction, token indices where needed)
- **Output**: `*ExactInQuote` with:
  - input/output amounts
  - execution price (WAD)
  - price impact (bps)
  - deterministic post-swap state
- **Error Model**: `Result<_, DexError>` with `DexError::MathError(MathError::...)` for arithmetic faults

### Uniswap V2

- **Module**: `dex::uniswap_v2::adapter_math`
- **Snapshot**: `V2PoolSnapshot { reserve0, reserve1, fee_bps }`
- **Quote API**:
  - `quote_exact_input(&V2PoolSnapshot, amount_in, SwapDirection) -> Result<V2ExactInQuote, DexError>`
- **Use case**: constant-product quoting and post-reserve updates for classic V2 pools.

### Uniswap V3

- **Module**: `dex::uniswap_v3::adapter_math`
- **Snapshot**: `V3PoolSnapshot { sqrt_price_x96, tick, liquidity, fee_bps, tick_spacing, initialized_ticks, tick_liquidity_net }`
- **Quote API**:
  - `quote_exact_input(&V3PoolSnapshot, amount_in, SwapDirection) -> Result<V3ExactInQuote, DexError>`
- **Use case**: concentrated-liquidity quoting with exact tick crossing and liquidityNet updates.
- **Fallback policy**: single-range path is allowed only when no crossing is mathematically proven.

### Balancer (Weighted)

- **Module**: `dex::balancer::adapter_math`
- **Snapshot**: `BalancerPoolSnapshot { balance0, balance1, weight0, weight1, swap_fee_bps }`
- **Quote API**:
  - `quote_exact_input(&BalancerPoolSnapshot, amount_in, SwapDirection) -> Result<BalancerExactInQuote, DexError>`
- **Use case**: weighted-pool exact-in quotes with deterministic post-balance state.

### Curve (StableSwap)

- **Module**: `dex::curve::adapter_math`
- **Snapshot**: `CurvePoolSnapshot { balances, decimals, stored_rates, variant, amplification, fee_raw, fee_bps }`
- **Quote API**:
  - `quote_exact_input(&CurvePoolSnapshot, token_in_index, token_out_index, amount_in) -> Result<CurveExactInQuote, DexError>`
- **Use case**: StableSwap exact-in output and post-trade balances across supported math variants.

### Kyber Elastic

- **Module**: `dex::kyber::adapter_math`
- **Snapshot**: `KyberPoolSnapshot { sqrt_price_x96, tick, liquidity, fee_bps, tick_spacing, initialized_ticks, tick_liquidity_net }`
- **Quote API**:
  - `quote_exact_input(&KyberPoolSnapshot, amount_in, SwapDirection) -> Result<KyberExactInQuote, DexError>`
- **Use case**: tick-aware exact-input quotes using Kyber swap-step semantics.
- **Fallback policy**: same safety model as V3 (no guessed fallback when crossing cannot be excluded).

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
