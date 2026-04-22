# Module Map

## `src/core`

- `error.rs`  
  Shared error types (`MathError`, `DexError`) used across math modules.
- `precision.rs`  
  Basis point utilities and fixed-point-safe helpers.
- `types.rs`  
  Domain identifiers (`DexType`, `PoolKey`).

## `src/data`

- `curve_registry.rs`  
  Embedded Curve registry schema and decoding helpers.
- `pool_state.rs`  
  Pool state shapes for V2/V3/Curve/Balancer.
- `kyber_pool_state.rs`  
  Kyber-specific pool state and tick/liquidity management helpers.
- `mod.rs`  
  `PoolState` exports and the `PoolStateProvider` trait abstraction.

## `src/dex`

- `adapter.rs`  
  Adapter trait and common swap result types.
- `common/mod.rs`  
  Shared conversion helpers (`ethers`/`alloy`) and exact-rate utility.

### Per-DEX Math

- `uniswap_v2/math.rs`  
  Canonical constant-product amount-out math.
- `uniswap_v3/math.rs`  
  Concentrated-liquidity math, tick stepping, and swap path utilities.
- `curve/math.rs`  
  StableSwap invariant and swap/price calculations.
- `curve/curve_math_pool.rs`  
  Bridge between internal curve pool state and `curve-math` pool models.
- `balancer/math.rs`  
  Weighted-pool spot and swap math using `balancer-maths-rust`.
- `balancer/conversions.rs`  
  Type conversion and error mapping helpers used by Balancer math.
- `kyber/math.rs`  
  Kyber Elastic tick/swap/liquidity math.

## Embedded Data

- `pool_resolver/curve_pool_registry.json`  
  Registry consumed by `src/data/curve_registry.rs`.
