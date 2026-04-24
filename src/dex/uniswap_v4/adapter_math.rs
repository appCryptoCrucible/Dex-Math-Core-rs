//! Decoupled, production-grade Uniswap v4 exact-in quoting adapter.
//!
//! Scope in this module is intentionally strict:
//! - deterministic no-hook static-fee pools
//! - deterministic fee-only hook mode when explicit metadata is provided
//! - fail-closed for unsupported hook/dynamic-fee behavior

use std::collections::HashMap;

use alloy_primitives::{Address, U256};
use uniswap_v3_math::{full_math, tick_math};

use crate::core::{BasisPoints, DexError, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::kyber::math::swap_math;
use crate::dex::uniswap_v3;

const BPS_DENOM_U32: u32 = 10_000;
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Canonical Uniswap v4 hook classes (official docs/examples scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V4OfficialHookClass {
    GeomeanOracle,
    VolatilityOracle,
    PointsHook,
    DynamicFee,
    LimitOrder,
    Twamm,
    AsyncSwap,
    CustomAccounting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V4HookSwapDeltas {
    /// Signed adjustment to gross input before core swap math.
    /// Positive increases effective swap input; negative decreases it.
    pub before_swap_input_delta: i128,
    /// Signed adjustment to output after core swap math.
    /// Positive increases amount_out; negative decreases amount_out.
    pub after_swap_output_delta: i128,
}

/// Built-in hook state model for pool-agnostic users.
///
/// This lets integrators provide hook state directly (pool + hook + state)
/// without implementing a custom adapter trait first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V4HookClassState {
    GeomeanOracle { quote_neutral: bool },
    VolatilityOracle { quote_neutral: bool },
    PointsHook { quote_neutral: bool },
    DynamicFee { effective_fee_bps: BasisPoints },
    LimitOrder {
        effective_fee_bps: BasisPoints,
        deltas: V4HookSwapDeltas,
    },
    Twamm {
        effective_fee_bps: BasisPoints,
        deltas: V4HookSwapDeltas,
    },
    AsyncSwap {
        effective_fee_bps: BasisPoints,
        deltas: V4HookSwapDeltas,
    },
    CustomAccounting {
        effective_fee_bps: BasisPoints,
        deltas: V4HookSwapDeltas,
    },
}

impl V4HookClassState {
    pub fn class(&self) -> V4OfficialHookClass {
        match self {
            Self::GeomeanOracle { .. } => V4OfficialHookClass::GeomeanOracle,
            Self::VolatilityOracle { .. } => V4OfficialHookClass::VolatilityOracle,
            Self::PointsHook { .. } => V4OfficialHookClass::PointsHook,
            Self::DynamicFee { .. } => V4OfficialHookClass::DynamicFee,
            Self::LimitOrder { .. } => V4OfficialHookClass::LimitOrder,
            Self::Twamm { .. } => V4OfficialHookClass::Twamm,
            Self::AsyncSwap { .. } => V4OfficialHookClass::AsyncSwap,
            Self::CustomAccounting { .. } => V4OfficialHookClass::CustomAccounting,
        }
    }
}

impl V4OfficialHookClass {
    fn parse(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "geomeanoracle" | "geomean_oracle" => Some(Self::GeomeanOracle),
            "volatilityoracle" | "volatility_oracle" => Some(Self::VolatilityOracle),
            "pointshook" | "points_hook" | "points" => Some(Self::PointsHook),
            "dynamicfee" | "dynamic_fee" => Some(Self::DynamicFee),
            "limitorder" | "limit_order" => Some(Self::LimitOrder),
            "twamm" => Some(Self::Twamm),
            "asyncswap" | "async_swap" => Some(Self::AsyncSwap),
            "customaccounting" | "custom_accounting" => Some(Self::CustomAccounting),
            _ => None,
        }
    }
}

/// Extensible adapter interface for hook classes that require class-specific state models.
///
/// Users can implement this trait in host applications and pass it to
/// `quote_exact_input_with_hook_adapter` for deterministic hook-aware quoting.
pub trait V4HookQuoteAdapter {
    fn class(&self) -> V4OfficialHookClass;

    fn resolve_effective_fee_bps(
        &self,
        pool: &V4PoolSnapshot,
        amount_in: U256,
        direction: SwapDirection,
    ) -> Result<BasisPoints, DexError>;

    fn adjust_exact_in_output(
        &self,
        _pool: &V4PoolSnapshot,
        _amount_in: U256,
        _direction: SwapDirection,
        core_amount_out: U256,
    ) -> Result<U256, DexError> {
        Ok(core_amount_out)
    }
}

/// Hook policy for deterministic v4 quoting.
#[derive(Debug, Clone)]
pub enum V4HookMode {
    /// No hooks and no dynamic fee paths.
    NoHooks,
    /// Hooks that are quote-neutral observers for swap output semantics.
    PassiveObserver { class: V4OfficialHookClass },
    /// Hook fee is deterministic and explicitly provided in snapshot metadata.
    DeterministicFeeOnly { class: V4OfficialHookClass, effective_fee_bps: BasisPoints },
    /// Hook class requires class-specific adapter/state modeling for exact quotes.
    RequiresExternalAdapter { class: V4OfficialHookClass, reason: String },
    /// Unsupported or unavailable hook behavior.
    Unsupported { reason: String },
}

/// Serializable v4 snapshot for deterministic adapter math.
#[derive(Debug, Clone)]
pub struct V4PoolSnapshot {
    pub hook_address: Option<Address>,
    pub hook_class: Option<V4OfficialHookClass>,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee_bps: BasisPoints,
    pub tick_spacing: i32,
    pub initialized_ticks: Vec<i32>,
    pub tick_liquidity_net: HashMap<i32, i128>,
    pub hook_mode: V4HookMode,
}

impl V4PoolSnapshot {
    fn effective_fee_bps(&self) -> Result<(BasisPoints, bool), DexError> {
        match &self.hook_mode {
            V4HookMode::NoHooks => Ok((self.fee_bps, false)),
            V4HookMode::PassiveObserver { .. } => Ok((self.fee_bps, false)),
            V4HookMode::DeterministicFeeOnly { effective_fee_bps, .. } => Ok((*effective_fee_bps, true)),
            V4HookMode::RequiresExternalAdapter { class, reason } => Err(DexError::InvalidPool {
                reason: format!("hook class {:?} requires external adapter: {}", class, reason),
            }),
            V4HookMode::Unsupported { reason } => Err(DexError::InvalidPool {
                reason: format!("unsupported v4 hook mode: {}", reason),
            }),
        }
    }

    fn validate_static(&self) -> Result<(), DexError> {
        if self.sqrt_price_x96.is_zero() {
            return Err(DexError::InvalidPool {
                reason: "sqrt_price_x96 cannot be zero".to_string(),
            });
        }
        if self.liquidity == 0 {
            return Err(DexError::InvalidPool {
                reason: "liquidity cannot be zero".to_string(),
            });
        }
        if self.tick_spacing <= 0 {
            return Err(DexError::InvalidPool {
                reason: format!("tick_spacing must be >0, got {}", self.tick_spacing),
            });
        }
        if self.initialized_ticks.windows(2).any(|w| w[0] >= w[1]) {
            return Err(DexError::InvalidPool {
                reason: "initialized_ticks must be strictly ascending".to_string(),
            });
        }
        if self.initialized_ticks.is_empty() {
            return Err(DexError::InvalidPool {
                reason: "initialized_ticks missing; exact v4 math unavailable".to_string(),
            });
        }
        if self.tick_liquidity_net.is_empty() {
            return Err(DexError::InvalidPool {
                reason: "tick_liquidity_net missing; exact v4 tick crossing unavailable".to_string(),
            });
        }
        let fee_to_check = match &self.hook_mode {
            V4HookMode::NoHooks | V4HookMode::PassiveObserver { .. } => self.fee_bps,
            V4HookMode::DeterministicFeeOnly { effective_fee_bps, .. } => *effective_fee_bps,
            V4HookMode::RequiresExternalAdapter { .. } => self.fee_bps,
            V4HookMode::Unsupported { reason } => {
                return Err(DexError::InvalidPool {
                    reason: format!("unsupported v4 hook mode: {}", reason),
                })
            }
        };
        if fee_to_check.as_u32() >= BPS_DENOM_U32 {
            return Err(DexError::InvalidPool {
                reason: format!("fee_bps must be <10000, got {}", fee_to_check.as_u32()),
            });
        }
        let decoded_tick = uniswap_v3::math::sqrt_price_to_tick(self.sqrt_price_x96).map_err(DexError::MathError)?;
        let tick_delta = ((decoded_tick as i64) - (self.tick as i64)).abs();
        if tick_delta > self.tick_spacing as i64 {
            return Err(DexError::InvalidPool {
                reason: format!(
                    "tick/sqrt_price_x96 mismatch: tick={}, decoded_tick={}, spacing={}",
                    self.tick, decoded_tick, self.tick_spacing
                ),
            });
        }
        Ok(())
    }
}

impl TryFrom<&crate::data::pool_state::V4PoolState> for V4PoolSnapshot {
    type Error = DexError;

    fn try_from(v: &crate::data::pool_state::V4PoolState) -> Result<Self, Self::Error> {
        let initialized_ticks = if v.initialized_ticks.windows(2).all(|w| w[0] < w[1]) {
            v.initialized_ticks.clone()
        } else {
            let mut ticks = v.initialized_ticks.clone();
            ticks.sort_unstable();
            ticks.dedup();
            ticks
        };

        let parsed_hook_class = v
            .hook_class
            .as_ref()
            .and_then(|label| V4OfficialHookClass::parse(label));

        let hook_mode = if !v.hooks_enabled && !v.dynamic_fee_enabled {
            V4HookMode::NoHooks
        } else {
            let class = parsed_hook_class.ok_or_else(|| DexError::InvalidPool {
                reason: format!(
                    "hooks enabled but hook_class missing/unknown (hook_class={:?})",
                    v.hook_class
                ),
            })?;
            match class {
                V4OfficialHookClass::GeomeanOracle
                | V4OfficialHookClass::VolatilityOracle
                | V4OfficialHookClass::PointsHook => V4HookMode::PassiveObserver { class },
                V4OfficialHookClass::DynamicFee => {
                    let deterministic_fee_bps = v.deterministic_fee_bps.ok_or_else(|| DexError::InvalidPool {
                        reason: "dynamic fee hook requires deterministic_fee_bps metadata".to_string(),
                    })?;
                    if deterministic_fee_bps >= 10_000 {
                        return Err(DexError::InvalidPool {
                            reason: format!(
                                "deterministic_fee_bps must be <10000, got {}",
                                deterministic_fee_bps
                            ),
                        });
                    }
                    V4HookMode::DeterministicFeeOnly {
                        class,
                        effective_fee_bps: BasisPoints::new_const(deterministic_fee_bps),
                    }
                }
                V4OfficialHookClass::LimitOrder
                | V4OfficialHookClass::Twamm
                | V4OfficialHookClass::AsyncSwap
                | V4OfficialHookClass::CustomAccounting => V4HookMode::RequiresExternalAdapter {
                    class,
                    reason: "class-specific hook state/execution model required for exact output".to_string(),
                },
            }
        };

        Ok(Self {
            hook_address: v.hook_address.map(|addr| Address::from_slice(addr.as_bytes())),
            hook_class: parsed_hook_class,
            sqrt_price_x96: crate::dex::common::ethers_to_alloy(v.sqrt_price_x96),
            tick: v.tick,
            liquidity: v.liquidity,
            fee_bps: BasisPoints::new_const(v.fee_bps),
            tick_spacing: v.tick_spacing,
            initialized_ticks,
            tick_liquidity_net: v.tick_liquidity_map.clone(),
            hook_mode,
        })
    }
}

/// Exact-input quote result with post-state and diagnostics.
#[derive(Debug, Clone)]
pub struct V4ExactInQuote {
    pub amount_in: U256,
    pub amount_in_effective: U256,
    pub amount_in_after_fee: U256,
    pub amount_out: U256,
    pub execution_price_wad: U256,
    pub price_impact_bps: u32,
    pub sqrt_price_before_x96: U256,
    pub sqrt_price_after_x96: U256,
    pub tick_before: i32,
    pub tick_after: i32,
    pub liquidity_before: u128,
    pub liquidity_after: u128,
    pub crossed_ticks: Vec<i32>,
    pub fee_bps_applied: BasisPoints,
    pub used_deterministic_fee_override: bool,
    pub hook_input_delta: i128,
    pub hook_output_delta: i128,
    pub used_hook_state: bool,
}

#[inline(always)]
fn apply_fee_exact_in(amount_in: U256, fee_bps: BasisPoints) -> Result<U256, MathError> {
    let fee_amount = uniswap_v3::math::mul_div_rounding_up(amount_in, U256::from(fee_bps.as_u32()), BPS_DENOM)?;
    amount_in.checked_sub(fee_amount).ok_or_else(|| MathError::Underflow {
        operation: "v4.apply_fee_exact_in".to_string(),
        inputs: vec![],
        context: format!("amount_in={}, fee_bps={}, fee_amount={}", amount_in, fee_bps.as_u32(), fee_amount),
    })
}

#[inline(always)]
fn execution_price_wad(amount_in: U256, amount_out: U256, direction: SwapDirection) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "v4.execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    match direction {
        SwapDirection::Token0ToToken1 => {
            full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
                operation: "v4.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if amount_out.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "v4.execution_price_wad".to_string(),
                    context: "amount_out".to_string(),
                });
            }
            full_math::mul_div(amount_in, WAD, amount_out).map_err(|e| MathError::Overflow {
                operation: "v4.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("inverse mul_div failed: {}", e),
            })
        }
    }
}

#[inline(always)]
fn exact_input_amount_out_from_returned(returned_amount: i128, context: &str) -> Result<U256, DexError> {
    let neg = returned_amount.checked_neg().ok_or_else(|| DexError::MathError(MathError::Overflow {
        operation: "v4.quote_exact_input.returned_amount".to_string(),
        inputs: vec![],
        context: format!("checked_neg overflow ({})", context),
    }))?;
    Ok(U256::from(neg as u128))
}

fn effective_fee_with_adapter(
    pool: &V4PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
    hook_adapter: Option<&dyn V4HookQuoteAdapter>,
) -> Result<(BasisPoints, bool), DexError> {
    if let (Some(class), Some(adapter)) = (pool.hook_class, hook_adapter) {
        if adapter.class() != class {
            return Err(DexError::InvalidPool {
                reason: format!(
                    "hook adapter class mismatch: snapshot={:?}, adapter={:?}",
                    class,
                    adapter.class()
                ),
            });
        }
    }

    match &pool.hook_mode {
        V4HookMode::NoHooks | V4HookMode::PassiveObserver { .. } => Ok((pool.fee_bps, false)),
        V4HookMode::DeterministicFeeOnly { effective_fee_bps, .. } => Ok((*effective_fee_bps, true)),
        V4HookMode::RequiresExternalAdapter { class, reason } => {
            let adapter = hook_adapter.ok_or_else(|| DexError::InvalidPool {
                reason: format!(
                    "hook class {:?} requires external adapter: {}",
                    class, reason
                ),
            })?;
            let fee = adapter.resolve_effective_fee_bps(pool, amount_in, direction)?;
            if fee.as_u32() >= BPS_DENOM_U32 {
                return Err(DexError::InvalidPool {
                    reason: format!("external adapter returned invalid fee_bps {}", fee.as_u32()),
                });
            }
            Ok((fee, true))
        }
        V4HookMode::Unsupported { reason } => Err(DexError::InvalidPool {
            reason: format!("unsupported v4 hook mode: {}", reason),
        }),
    }
}

fn hook_effects_from_state(
    pool: &V4PoolSnapshot,
    state: &V4HookClassState,
) -> Result<(BasisPoints, i128, i128, bool), DexError> {
    if let Some(pool_class) = pool.hook_class {
        if pool_class != state.class() {
            return Err(DexError::InvalidPool {
                reason: format!(
                    "hook state class mismatch: snapshot={:?}, state={:?}",
                    pool_class,
                    state.class()
                ),
            });
        }
    } else {
        return Err(DexError::InvalidPool {
            reason: "hook_state provided but snapshot hook_class is missing".to_string(),
        });
    }

    match state {
        V4HookClassState::GeomeanOracle { quote_neutral }
        | V4HookClassState::VolatilityOracle { quote_neutral }
        | V4HookClassState::PointsHook { quote_neutral } => {
            if !quote_neutral {
                return Err(DexError::InvalidPool {
                    reason: format!(
                        "hook class {:?} declared non-neutral without explicit deltas",
                        state.class()
                    ),
                });
            }
            Ok((pool.fee_bps, 0, 0, false))
        }
        V4HookClassState::DynamicFee { effective_fee_bps } => Ok((*effective_fee_bps, 0, 0, true)),
        V4HookClassState::LimitOrder {
            effective_fee_bps,
            deltas,
        }
        | V4HookClassState::Twamm {
            effective_fee_bps,
            deltas,
        }
        | V4HookClassState::AsyncSwap {
            effective_fee_bps,
            deltas,
        }
        | V4HookClassState::CustomAccounting {
            effective_fee_bps,
            deltas,
        } => Ok((
            *effective_fee_bps,
            deltas.before_swap_input_delta,
            deltas.after_swap_output_delta,
            true,
        )),
    }
}

#[inline(always)]
fn apply_signed_delta_to_u256(base: U256, delta: i128, context: &str) -> Result<U256, DexError> {
    if delta >= 0 {
        base.checked_add(U256::from(delta as u128))
            .ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.apply_signed_delta_to_u256".to_string(),
                inputs: vec![],
                context: format!("{} add delta {}", context, delta),
            }))
    } else {
        let abs = delta
            .checked_abs()
            .ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.apply_signed_delta_to_u256".to_string(),
                inputs: vec![],
                context: format!(
                    "{} abs overflow for delta {} (i128::MIN two's-complement edge case)",
                    context, delta
                ),
            }))?;
        base.checked_sub(U256::from(abs as u128))
            .ok_or_else(|| DexError::MathError(MathError::Underflow {
                operation: "v4.apply_signed_delta_to_u256".to_string(),
                inputs: vec![],
                context: format!("{} subtract delta {}", context, delta),
            }))
    }
}

#[inline(always)]
fn find_next_initialized_tick(
    current_tick: i32,
    initialized_ticks: &[i32],
    tick_spacing: i32,
    zero_for_one: bool,
) -> Result<i32, MathError> {
    if tick_spacing <= 0 {
        return Err(MathError::InvalidInput {
            operation: "v4.find_next_initialized_tick".to_string(),
            reason: "tick_spacing must be positive".to_string(),
            context: format!("tick_spacing={}", tick_spacing),
        });
    }
    if initialized_ticks.is_empty() {
        return Err(MathError::InvalidInput {
            operation: "v4.find_next_initialized_tick".to_string(),
            reason: "no initialized ticks".to_string(),
            context: "initialized_ticks empty".to_string(),
        });
    }
    if zero_for_one {
        let pos = initialized_ticks.partition_point(|&t| t < current_tick);
        if pos > 0 {
            Ok(initialized_ticks[pos - 1])
        } else {
            Ok(uniswap_v3::math::MIN_TICK)
        }
    } else {
        let pos = initialized_ticks.partition_point(|&t| t <= current_tick);
        if pos < initialized_ticks.len() {
            Ok(initialized_ticks[pos])
        } else {
            Ok(uniswap_v3::math::MAX_TICK)
        }
    }
}

#[inline(always)]
fn init_tick_cursor(current_tick: i32, initialized_ticks: &[i32], zero_for_one: bool) -> usize {
    if zero_for_one {
        initialized_ticks.partition_point(|&t| t < current_tick)
    } else {
        initialized_ticks.partition_point(|&t| t <= current_tick)
    }
}

#[inline(always)]
fn next_tick_from_cursor(initialized_ticks: &[i32], cursor: usize, zero_for_one: bool) -> i32 {
    if zero_for_one {
        if cursor > 0 {
            initialized_ticks[cursor - 1]
        } else {
            uniswap_v3::math::MIN_TICK
        }
    } else if cursor < initialized_ticks.len() {
        initialized_ticks[cursor]
    } else {
        uniswap_v3::math::MAX_TICK
    }
}

/// Deterministic exact-input quote for Uniswap v4 concentrated liquidity (strict mode).
///
/// Canonical backend:
/// - Uses `uniswap_v3_math`-equivalent swap step semantics through the kyber swap_math wrapper.
/// - This is valid for v4 no-hook pools because concentrated-liquidity math and rounding
///   semantics remain aligned when no custom hook behavior mutates swap outcomes.
pub fn quote_exact_input(
    pool: &V4PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
) -> Result<V4ExactInQuote, DexError> {
    quote_exact_input_with_hook_state(pool, amount_in, direction, None)
}

/// Deterministic exact-input quote with built-in official hook state input.
pub fn quote_exact_input_with_hook_state(
    pool: &V4PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
    hook_state: Option<&V4HookClassState>,
) -> Result<V4ExactInQuote, DexError> {
    quote_exact_input_with_hook_adapter_and_state(pool, amount_in, direction, None, hook_state)
}

/// Deterministic exact-input quote with optional external hook adapter.
pub fn quote_exact_input_with_hook_adapter(
    pool: &V4PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
    hook_adapter: Option<&dyn V4HookQuoteAdapter>,
) -> Result<V4ExactInQuote, DexError> {
    quote_exact_input_with_hook_adapter_and_state(pool, amount_in, direction, hook_adapter, None)
}

/// Deterministic exact-input quote with both extensibility inputs:
/// - `hook_state` for built-in official class handling
/// - `hook_adapter` for custom adapter logic
pub fn quote_exact_input_with_hook_adapter_and_state(
    pool: &V4PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
    hook_adapter: Option<&dyn V4HookQuoteAdapter>,
    hook_state: Option<&V4HookClassState>,
) -> Result<V4ExactInQuote, DexError> {
    pool.validate_static()?;
    if hook_state.is_some() && hook_adapter.is_some() {
        return Err(DexError::InvalidPool {
            reason: "cannot provide both hook_state and hook_adapter".to_string(),
        });
    }
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "v4.quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "v4 adapter".to_string(),
        }));
    }

    let (state_fee, hook_input_delta, hook_output_delta, used_hook_state) = if let Some(state) = hook_state {
        let (fee, input_delta, output_delta, used) = hook_effects_from_state(pool, state)?;
        (Some(fee), input_delta, output_delta, used)
    } else {
        (None, 0, 0, false)
    };
    let amount_in_effective =
        apply_signed_delta_to_u256(amount_in, hook_input_delta, "hook before_swap input delta")?;

    let (fee_bps_applied, used_override) = if let Some(fee) = state_fee {
        (fee, used_hook_state)
    } else {
        effective_fee_with_adapter(pool, amount_in_effective, direction, hook_adapter)?
    };

    let amount_in_after_fee = apply_fee_exact_in(amount_in_effective, fee_bps_applied).map_err(DexError::MathError)?;
    if amount_in_after_fee.is_zero() {
        return Ok(V4ExactInQuote {
            amount_in,
            amount_in_effective,
            amount_in_after_fee,
            amount_out: U256::ZERO,
            execution_price_wad: U256::ZERO,
            price_impact_bps: 0,
            sqrt_price_before_x96: pool.sqrt_price_x96,
            sqrt_price_after_x96: pool.sqrt_price_x96,
            tick_before: pool.tick,
            tick_after: pool.tick,
            liquidity_before: pool.liquidity,
            liquidity_after: pool.liquidity,
            crossed_ticks: Vec::new(),
            fee_bps_applied,
            used_deterministic_fee_override: used_override,
            hook_input_delta,
            hook_output_delta,
            used_hook_state,
        });
    }

    let zero_for_one = matches!(direction, SwapDirection::Token0ToToken1);

    let mut remaining = amount_in_effective;
    let mut amount_out_total = U256::ZERO;
    let mut current_sqrt = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut current_liquidity = pool.liquidity;
    let mut crossed_ticks = Vec::with_capacity(pool.initialized_ticks.len().min(8));
    let mut tick_cursor = init_tick_cursor(current_tick, &pool.initialized_ticks, zero_for_one);
    let fee_bps_u32 = fee_bps_applied.as_u32();

    for _ in 0..1024usize {
        if remaining.is_zero() {
            break;
        }
        let next_tick = next_tick_from_cursor(&pool.initialized_ticks, tick_cursor, zero_for_one);
        let target = tick_math::get_sqrt_ratio_at_tick(next_tick).map_err(|e| {
            DexError::MathError(MathError::InvalidInput {
                operation: "v4.quote_exact_input.get_sqrt_ratio_at_tick".to_string(),
                reason: format!("{}", e),
                context: format!("next_tick={}", next_tick),
            })
        })?;
        let specified_i128 = i128::try_from(remaining).map_err(|_| DexError::InvalidPool {
            reason: "remaining amount exceeds i128 range required by swap step".to_string(),
        })?;
        let step = swap_math::compute_swap_step(
            current_liquidity,
            current_sqrt,
            target,
            fee_bps_u32,
            specified_i128,
            true,
            zero_for_one,
        )
        .map_err(DexError::MathError)?;
        if step.used_amount < 0 {
            return Err(DexError::InvalidPool {
                reason: "swap step returned negative used_amount".to_string(),
            });
        }
        if step.returned_amount > 0 {
            return Err(DexError::InvalidPool {
                reason: "swap step returned positive returned_amount in exact-input mode".to_string(),
            });
        }

        let used = U256::from(step.used_amount as u128);
        if used.is_zero() && !remaining.is_zero() {
            return Err(DexError::InvalidPool {
                reason: "swap step made no progress while remaining input is non-zero".to_string(),
            });
        }
        let out = exact_input_amount_out_from_returned(step.returned_amount, "tick-crossing loop")?;

        amount_out_total = amount_out_total
            .checked_add(out)
            .ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.quote_exact_input.amount_out_total".to_string(),
                inputs: vec![],
                context: "accumulate output".to_string(),
            }))?;
        remaining = remaining
            .checked_sub(used)
            .ok_or_else(|| DexError::MathError(MathError::Underflow {
                operation: "v4.quote_exact_input.remaining".to_string(),
                inputs: vec![],
                context: "remaining - used".to_string(),
            }))?;
        current_sqrt = step.next_sqrt_p;

        if step.next_sqrt_p == target && !remaining.is_zero() {
            let liq_net = pool.tick_liquidity_net.get(&next_tick).ok_or_else(|| DexError::InvalidPool {
                reason: format!("missing liquidityNet for crossed tick {}", next_tick),
            })?;
            let l = current_liquidity as i128;
            let liq_signed = if zero_for_one { -*liq_net } else { *liq_net };
            let new_l = l.checked_add(liq_signed).ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.quote_exact_input.liquidity_update".to_string(),
                inputs: vec![],
                context: format!("l={}, liq_net={}, zero_for_one={}", l, liq_net, zero_for_one),
            }))?;
            if new_l < 0 {
                return Err(DexError::InvalidPool {
                    reason: format!("negative active liquidity after crossing tick {}", next_tick),
                });
            }
            current_liquidity = u128::try_from(new_l).map_err(|_| DexError::InvalidPool {
                reason: format!("liquidity overflow after crossing tick {}", next_tick),
            })?;
            crossed_ticks.push(next_tick);
            if zero_for_one {
                if tick_cursor > 0 {
                    tick_cursor -= 1;
                }
                current_tick = next_tick.saturating_sub(1);
            } else {
                if tick_cursor < pool.initialized_ticks.len() {
                    tick_cursor += 1;
                }
                current_tick = next_tick;
            }
            if current_liquidity == 0 && !remaining.is_zero() {
                return Err(DexError::InvalidPool {
                    reason: "liquidity became zero before amount was exhausted".to_string(),
                });
            }
        } else {
            current_tick = uniswap_v3::math::sqrt_price_to_tick(current_sqrt).map_err(DexError::MathError)?;
            break;
        }
    }
    if !remaining.is_zero() {
        return Err(DexError::InvalidPool {
            reason: "swap exceeded 1024 tick-crossings; quote incomplete".to_string(),
        });
    }

    let mut amount_out_total = if let (Some(adapter), Some(_class)) = (hook_adapter, pool.hook_class) {
        adapter.adjust_exact_in_output(pool, amount_in, direction, amount_out_total)?
    } else {
        amount_out_total
    };
    amount_out_total = apply_signed_delta_to_u256(
        amount_out_total,
        hook_output_delta,
        "hook after_swap output delta",
    )?;

    let execution =
        execution_price_wad(amount_in_after_fee, amount_out_total, direction).map_err(DexError::MathError)?;
    let impact = uniswap_v3::math::calculate_v3_price_impact(pool.sqrt_price_x96, current_sqrt)
        .map_err(DexError::MathError)?;

    Ok(V4ExactInQuote {
        amount_in,
        amount_in_effective,
        amount_in_after_fee,
        amount_out: amount_out_total,
        execution_price_wad: execution,
        price_impact_bps: impact,
        sqrt_price_before_x96: pool.sqrt_price_x96,
        sqrt_price_after_x96: current_sqrt,
        tick_before: pool.tick,
        tick_after: current_tick,
        liquidity_before: pool.liquidity,
        liquidity_after: current_liquidity,
        crossed_ticks,
        fee_bps_applied,
        used_deterministic_fee_override: used_override,
        hook_input_delta,
        hook_output_delta,
        used_hook_state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::I256;
    use uniswap_v3_math::swap_math as canonical_swap_math;

    #[derive(Clone)]
    struct ParityFixture {
        pool: V4PoolSnapshot,
        amount_in: U256,
        direction: SwapDirection,
    }

    fn parity_fixtures() -> Vec<ParityFixture> {
        vec![
            ParityFixture {
                pool: V4PoolSnapshot {
                    hook_address: None,
                    hook_class: None,
                    sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
                    tick: 0,
                    liquidity: 1_000_000_000_000u128,
                    fee_bps: BasisPoints::new_const(30),
                    tick_spacing: 60,
                    initialized_ticks: vec![-120, -60, 0, 60, 120],
                    tick_liquidity_net: HashMap::from([(60, -200_000i128), (120, -150_000i128)]),
                    hook_mode: V4HookMode::NoHooks,
                },
                amount_in: U256::from(100_000u64),
                direction: SwapDirection::Token1ToToken0,
            },
            ParityFixture {
                pool: V4PoolSnapshot {
                    hook_address: None,
                    hook_class: None,
                    sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
                    tick: 0,
                    liquidity: 1_000_000_000_000u128,
                    fee_bps: BasisPoints::new_const(30),
                    tick_spacing: 60,
                    initialized_ticks: vec![-120, -60, 0, 60, 120],
                    tick_liquidity_net: HashMap::from([(-60, 150_000i128), (-120, 150_000i128)]),
                    hook_mode: V4HookMode::NoHooks,
                },
                amount_in: U256::from(95_000u64),
                direction: SwapDirection::Token0ToToken1,
            },
            ParityFixture {
                pool: V4PoolSnapshot {
                    hook_address: None,
                    hook_class: Some(V4OfficialHookClass::DynamicFee),
                    sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
                    tick: 0,
                    liquidity: 1_000_000_000_000u128,
                    fee_bps: BasisPoints::new_const(30),
                    tick_spacing: 60,
                    initialized_ticks: vec![-120, -60, 0, 60, 120],
                    tick_liquidity_net: HashMap::from([(60, -200_000i128)]),
                    hook_mode: V4HookMode::DeterministicFeeOnly {
                        class: V4OfficialHookClass::DynamicFee,
                        effective_fee_bps: BasisPoints::new_const(45),
                    },
                },
                amount_in: U256::from(50_000u64),
                direction: SwapDirection::Token1ToToken0,
            },
        ]
    }

    fn abs_diff(a: U256, b: U256) -> U256 {
        if a >= b { a - b } else { b - a }
    }

    fn map_canonical_err(operation: &str, err: impl std::fmt::Display) -> DexError {
        DexError::MathError(MathError::InvalidInput {
            operation: operation.to_string(),
            reason: format!("{}", err),
            context: "canonical_swap_math".to_string(),
        })
    }

    // On-chain parity harness (fixture replay): canonical concentrated-liquidity step semantics.
    fn reference_quote_exact_input(pool: &V4PoolSnapshot, amount_in: U256, direction: SwapDirection) -> Result<U256, DexError> {
        pool.validate_static()?;
        let (effective_fee, _) = pool.effective_fee_bps()?;
        let fee_pips = effective_fee.as_u32().checked_mul(100).ok_or_else(|| DexError::MathError(MathError::Overflow {
            operation: "v4.reference_quote_exact_input".to_string(),
            inputs: vec![],
            context: "fee bps -> fee pips".to_string(),
        }))?;

        let zero_for_one = matches!(direction, SwapDirection::Token0ToToken1);
        let mut remaining = amount_in;
        let mut out_total = U256::ZERO;
        let mut current_sqrt = pool.sqrt_price_x96;
        let mut current_tick = pool.tick;
        let mut current_liquidity = pool.liquidity;

        for _ in 0..1024usize {
            if remaining.is_zero() {
                break;
            }
            let next_tick = find_next_initialized_tick(
                current_tick,
                &pool.initialized_ticks,
                pool.tick_spacing,
                zero_for_one,
            )
            .map_err(DexError::MathError)?;
            let target = tick_math::get_sqrt_ratio_at_tick(next_tick).map_err(|e| {
                DexError::MathError(MathError::InvalidInput {
                    operation: "v4.reference_quote_exact_input.get_sqrt_ratio_at_tick".to_string(),
                    reason: format!("{}", e),
                    context: format!("next_tick={}", next_tick),
                })
            })?;
            let amount_remaining = I256::from_raw(remaining);
            let (next_sqrt, amount_in_net, amount_out, fee_amount) = canonical_swap_math::compute_swap_step(
                current_sqrt,
                target,
                current_liquidity,
                amount_remaining,
                fee_pips,
            )
            .map_err(|e| map_canonical_err("v4.reference_quote_exact_input.step", e))?;

            let used = amount_in_net.checked_add(fee_amount).ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.reference_quote_exact_input".to_string(),
                inputs: vec![],
                context: "amount_in + fee".to_string(),
            }))?;
            if used > remaining {
                return Err(DexError::InvalidPool {
                    reason: format!("canonical step used {} > remaining {}", used, remaining),
                });
            }
            remaining -= used;
            out_total = out_total.checked_add(amount_out).ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "v4.reference_quote_exact_input".to_string(),
                inputs: vec![],
                context: "accumulate amount_out".to_string(),
            }))?;
            current_sqrt = next_sqrt;
            current_tick = uniswap_v3::math::sqrt_price_to_tick(current_sqrt).map_err(DexError::MathError)?;

            if next_sqrt == target && !remaining.is_zero() {
                let liq_net = pool.tick_liquidity_net.get(&next_tick).ok_or_else(|| DexError::InvalidPool {
                    reason: format!("missing liquidityNet for crossed tick {}", next_tick),
                })?;
                let l = current_liquidity as i128;
                let liq_signed = if zero_for_one { -*liq_net } else { *liq_net };
                let new_l = l.checked_add(liq_signed).ok_or_else(|| DexError::MathError(MathError::Overflow {
                    operation: "v4.reference_quote_exact_input.liquidity_update".to_string(),
                    inputs: vec![],
                    context: format!("l={}, liq_net={}, zero_for_one={}", l, liq_net, zero_for_one),
                }))?;
                if new_l < 0 {
                    return Err(DexError::InvalidPool {
                        reason: format!("negative liquidity after crossing tick {}", next_tick),
                    });
                }
                current_liquidity = new_l as u128;
            } else {
                break;
            }
        }

        Ok(out_total)
    }

    #[test]
    fn rejects_unsupported_hook_mode_without_deterministic_metadata() {
        let state = crate::data::pool_state::V4PoolState {
            pool_address: ethers_core::types::Address::zero(),
            token0: ethers_core::types::Address::zero(),
            token1: ethers_core::types::Address::zero(),
            liquidity: 1,
            sqrt_price_x96: ethers_core::types::U256::from(1u64),
            tick: 0,
            fee_bps: 30,
            tick_spacing: 60,
            last_update_block: 0,
            tick_liquidity_map: HashMap::from([(0, 1i128)]),
            initialized_ticks: vec![0, 60],
            hook_address: Some(ethers_core::types::Address::from_low_u64_be(1234)),
            hook_class: Some("twamm".to_string()),
            hooks_enabled: true,
            dynamic_fee_enabled: true,
            deterministic_fee_bps: None,
        };
        let snap = V4PoolSnapshot::try_from(&state).unwrap();
        let err = quote_exact_input(&snap, U256::from(100u64), SwapDirection::Token1ToToken0).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => {
                assert!(reason.contains("requires external adapter") || reason.contains("tick/sqrt_price_x96 mismatch"))
            }
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn allows_deterministic_fee_override_hook_mode() {
        let pool = V4PoolSnapshot {
            hook_address: None,
            hook_class: Some(V4OfficialHookClass::DynamicFee),
            sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(30),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60],
            tick_liquidity_net: HashMap::from([(60, -100_000i128)]),
            hook_mode: V4HookMode::DeterministicFeeOnly {
                class: V4OfficialHookClass::DynamicFee,
                effective_fee_bps: BasisPoints::new_const(45),
            },
        };
        let q = quote_exact_input(&pool, U256::from(10_000u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert!(q.used_deterministic_fee_override);
        assert_eq!(q.fee_bps_applied.as_u32(), 45);
    }

    #[test]
    fn rejects_missing_tick_liquidity_map() {
        let pool = V4PoolSnapshot {
            hook_address: None,
            hook_class: None,
            sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(30),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60],
            tick_liquidity_net: HashMap::new(),
            hook_mode: V4HookMode::NoHooks,
        };
        let err = quote_exact_input(&pool, U256::from(10_000u64), SwapDirection::Token1ToToken0).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("tick_liquidity_net")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[derive(Debug)]
    struct TwammMockAdapter {
        fee_bps: BasisPoints,
    }

    impl V4HookQuoteAdapter for TwammMockAdapter {
        fn class(&self) -> V4OfficialHookClass {
            V4OfficialHookClass::Twamm
        }

        fn resolve_effective_fee_bps(
            &self,
            _pool: &V4PoolSnapshot,
            _amount_in: U256,
            _direction: SwapDirection,
        ) -> Result<BasisPoints, DexError> {
            Ok(self.fee_bps)
        }
    }

    #[test]
    fn external_adapter_enables_twamm_class_quotes() {
        let state = crate::data::pool_state::V4PoolState {
            pool_address: ethers_core::types::Address::zero(),
            token0: ethers_core::types::Address::from_low_u64_be(1),
            token1: ethers_core::types::Address::from_low_u64_be(2),
            liquidity: 1_000_000_000_000u128,
            sqrt_price_x96: ethers_core::types::U256::from_dec_str("79228162514264337593543950336").unwrap(),
            tick: 0,
            fee_bps: 30,
            tick_spacing: 60,
            last_update_block: 1,
            tick_liquidity_map: HashMap::from([(60, -100_000i128)]),
            initialized_ticks: vec![-60, 0, 60],
            hook_address: Some(ethers_core::types::Address::from_low_u64_be(999)),
            hook_class: Some("twamm".to_string()),
            hooks_enabled: true,
            dynamic_fee_enabled: false,
            deterministic_fee_bps: None,
        };
        let pool = V4PoolSnapshot::try_from(&state).unwrap();

        let no_adapter_err =
            quote_exact_input_with_hook_adapter(&pool, U256::from(20_000u64), SwapDirection::Token1ToToken0, None)
                .unwrap_err();
        match no_adapter_err {
            DexError::InvalidPool { reason } => assert!(reason.contains("requires external adapter")),
            _ => panic!("expected InvalidPool"),
        }

        let adapter = TwammMockAdapter {
            fee_bps: BasisPoints::new_const(35),
        };
        let q = quote_exact_input_with_hook_adapter(
            &pool,
            U256::from(20_000u64),
            SwapDirection::Token1ToToken0,
            Some(&adapter),
        )
        .unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert_eq!(q.fee_bps_applied.as_u32(), 35);
    }

    #[test]
    fn passive_observer_hook_class_uses_core_math() {
        let state = crate::data::pool_state::V4PoolState {
            pool_address: ethers_core::types::Address::zero(),
            token0: ethers_core::types::Address::from_low_u64_be(1),
            token1: ethers_core::types::Address::from_low_u64_be(2),
            liquidity: 1_000_000_000_000u128,
            sqrt_price_x96: ethers_core::types::U256::from_dec_str("79228162514264337593543950336").unwrap(),
            tick: 0,
            fee_bps: 30,
            tick_spacing: 60,
            last_update_block: 1,
            tick_liquidity_map: HashMap::from([(60, -100_000i128)]),
            initialized_ticks: vec![-60, 0, 60],
            hook_address: Some(ethers_core::types::Address::from_low_u64_be(88)),
            hook_class: Some("volatility_oracle".to_string()),
            hooks_enabled: true,
            dynamic_fee_enabled: false,
            deterministic_fee_bps: None,
        };
        let pool = V4PoolSnapshot::try_from(&state).unwrap();
        let q = quote_exact_input(&pool, U256::from(20_000u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert_eq!(q.fee_bps_applied.as_u32(), 30);
        assert!(!q.used_deterministic_fee_override);
    }

    #[test]
    fn hook_state_enables_official_class_outputs_without_custom_adapter() {
        let pool = V4PoolSnapshot {
            hook_address: Some(Address::from([0x11; 20])),
            hook_class: Some(V4OfficialHookClass::Twamm),
            sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(30),
            tick_spacing: 60,
            initialized_ticks: vec![-120, -60, 0, 60, 120],
            tick_liquidity_net: HashMap::from([(60, -100_000i128)]),
            hook_mode: V4HookMode::RequiresExternalAdapter {
                class: V4OfficialHookClass::Twamm,
                reason: "state model required".to_string(),
            },
        };

        let state = V4HookClassState::Twamm {
            effective_fee_bps: BasisPoints::new_const(40),
            deltas: V4HookSwapDeltas {
                before_swap_input_delta: 1_000,
                after_swap_output_delta: -5,
            },
        };
        let q = quote_exact_input_with_hook_state(
            &pool,
            U256::from(25_000u64),
            SwapDirection::Token1ToToken0,
            Some(&state),
        )
        .unwrap();

        assert!(q.amount_out > U256::ZERO);
        assert_eq!(q.fee_bps_applied.as_u32(), 40);
        assert_eq!(q.hook_input_delta, 1_000);
        assert_eq!(q.hook_output_delta, -5);
        assert!(q.used_hook_state);
        assert_eq!(q.amount_in_effective, U256::from(26_000u64));
    }

    #[test]
    fn rejects_hook_state_class_mismatch() {
        let pool = V4PoolSnapshot {
            hook_address: Some(Address::from([0x22; 20])),
            hook_class: Some(V4OfficialHookClass::LimitOrder),
            sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(30),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60],
            tick_liquidity_net: HashMap::from([(60, -100_000i128)]),
            hook_mode: V4HookMode::RequiresExternalAdapter {
                class: V4OfficialHookClass::LimitOrder,
                reason: "state model required".to_string(),
            },
        };
        let mismatched = V4HookClassState::Twamm {
            effective_fee_bps: BasisPoints::new_const(35),
            deltas: V4HookSwapDeltas {
                before_swap_input_delta: 0,
                after_swap_output_delta: 0,
            },
        };
        let err = quote_exact_input_with_hook_state(
            &pool,
            U256::from(10_000u64),
            SwapDirection::Token1ToToken0,
            Some(&mismatched),
        )
        .unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("class mismatch")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn parity_harness_matches_canonical_within_one_wei() {
        for fixture in parity_fixtures() {
            let local = quote_exact_input(&fixture.pool, fixture.amount_in, fixture.direction).unwrap();
            let reference = reference_quote_exact_input(&fixture.pool, fixture.amount_in, fixture.direction).unwrap();
            let diff = abs_diff(local.amount_out, reference);
            assert!(diff <= U256::from(1u64), "parity diff exceeded 1 wei: {}", diff);
        }
    }
}
