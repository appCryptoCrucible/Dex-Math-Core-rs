//! Decoupled, production-grade Uniswap V3 exact-in quoting adapter.
//!
//! This module intentionally contains no sidecar runtime plumbing (no RPC,
//! no pool manager, no strategy dependencies). It operates on explicit pool
//! snapshots and returns deterministic quote outputs and post-trade state.

use std::collections::HashMap;

use alloy_primitives::U256;
use uniswap_v3_math::{full_math, sqrt_price_math};

use crate::core::{BasisPoints, DexError, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::common::ethers_to_alloy;
use crate::dex::uniswap_v3::math;

const BPS_DENOM_U32: u32 = 10_000;
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Serializable input snapshot for deterministic V3 math quoting.
#[derive(Debug, Clone)]
pub struct V3PoolSnapshot {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee_bps: BasisPoints,
    pub tick_spacing: i32,
    pub initialized_ticks: Vec<i32>,
    pub tick_liquidity_net: HashMap<i32, i128>,
}

impl V3PoolSnapshot {
    /// Validates static pool parameters required for exact math.
    fn validate_static(&self) -> Result<(), DexError> {
        if self.liquidity == 0 {
            return Err(DexError::InvalidPool {
                reason: "liquidity cannot be zero".to_string(),
            });
        }
        if self.tick_spacing <= 0 {
            return Err(DexError::InvalidPool {
                reason: format!("tick_spacing must be > 0, got {}", self.tick_spacing),
            });
        }
        if self.fee_bps.as_u32() >= BPS_DENOM_U32 {
            return Err(DexError::InvalidPool {
                reason: format!("fee_bps must be < 10000, got {}", self.fee_bps.as_u32()),
            });
        }
        if self.initialized_ticks.windows(2).any(|w| w[0] >= w[1]) {
            return Err(DexError::InvalidPool {
                reason: "initialized_ticks must be strictly ascending".to_string(),
            });
        }
        // Validate sqrt range and consistency with tick math.
        math::sqrt_price_to_tick(self.sqrt_price_x96).map_err(DexError::MathError)?;
        Ok(())
    }
}

impl From<&crate::data::pool_state::V3PoolState> for V3PoolSnapshot {
    fn from(v: &crate::data::pool_state::V3PoolState) -> Self {
        Self {
            sqrt_price_x96: ethers_to_alloy(v.sqrt_price_x96),
            tick: v.tick,
            liquidity: v.liquidity,
            fee_bps: BasisPoints::new_const(v.fee_tier),
            tick_spacing: v.tick_spacing,
            initialized_ticks: v.initialized_ticks.clone(),
            tick_liquidity_net: v.tick_liquidity_map.clone(),
        }
    }
}

/// Exact-in quote result including post-swap state and diagnostics.
#[derive(Debug, Clone)]
pub struct V3ExactInQuote {
    pub amount_in: U256,
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
    pub used_single_range_fallback: bool,
}

#[inline(always)]
fn apply_fee_exact_in(amount_in: U256, fee_bps: BasisPoints) -> Result<U256, MathError> {
    let multiplier = U256::from(BPS_DENOM_U32 - fee_bps.as_u32());
    amount_in
        .checked_mul(multiplier)
        .and_then(|v| v.checked_div(BPS_DENOM))
        .ok_or_else(|| MathError::Overflow {
            operation: "apply_fee_exact_in".to_string(),
            inputs: vec![crate::dex::common::alloy_to_ethers(amount_in)],
            context: format!("fee_bps={}", fee_bps.as_u32()),
        })
}

#[inline(always)]
fn execution_price_wad(amount_in: U256, amount_out: U256, direction: SwapDirection) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    match direction {
        SwapDirection::Token0ToToken1 => {
            full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
                operation: "execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("token0->token1 mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if amount_out.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "execution_price_wad".to_string(),
                    context: "amount_out".to_string(),
                });
            }
            full_math::mul_div(amount_in, WAD, amount_out).map_err(|e| MathError::Overflow {
                operation: "execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("token1->token0 mul_div failed: {}", e),
            })
        }
    }
}

fn find_next_initialized_tick(
    current_tick: i32,
    initialized_ticks: &[i32],
    tick_spacing: i32,
    zero_for_one: bool,
) -> Result<i32, MathError> {
    if tick_spacing <= 0 {
        return Err(MathError::InvalidInput {
            operation: "find_next_initialized_tick".to_string(),
            reason: "tick_spacing must be > 0".to_string(),
            context: format!("tick_spacing={}", tick_spacing),
        });
    }
    if zero_for_one {
        let pos = initialized_ticks.partition_point(|&t| t < current_tick);
        if pos > 0 {
            Ok(initialized_ticks[pos - 1])
        } else {
            Ok((current_tick.div_euclid(tick_spacing) - 1) * tick_spacing)
        }
    } else {
        let pos = initialized_ticks.partition_point(|&t| t <= current_tick);
        if pos < initialized_ticks.len() {
            Ok(initialized_ticks[pos])
        } else {
            Ok((current_tick.div_euclid(tick_spacing) + 1) * tick_spacing)
        }
    }
}

fn validate_single_range_fallback(
    pool: &V3PoolSnapshot,
    amount_in_after_fee: U256,
    direction: SwapDirection,
) -> Result<i32, DexError> {
    let zero_for_one = matches!(direction, SwapDirection::Token0ToToken1);
    let next_tick = find_next_initialized_tick(
        pool.tick,
        &pool.initialized_ticks,
        pool.tick_spacing,
        zero_for_one,
    )
    .map_err(DexError::MathError)?;
    let next_sqrt = math::get_sqrt_ratio_at_tick(next_tick).map_err(DexError::MathError)?;
    let max_to_next = if zero_for_one {
        math::get_amount0_delta(next_sqrt, pool.sqrt_price_x96, pool.liquidity, true)
            .map_err(DexError::MathError)?
    } else {
        math::get_amount1_delta(pool.sqrt_price_x96, next_sqrt, pool.liquidity, true)
            .map_err(DexError::MathError)?
    };
    if amount_in_after_fee < max_to_next {
        Ok(next_tick)
    } else {
        Err(DexError::InvalidPool {
            reason: format!(
                "single-range fallback rejected; possible tick crossing (tick={}, next_tick={})",
                pool.tick, next_tick
            ),
        })
    }
}

/// Deterministic exact-in quote with fail-closed tick crossing guarantees.
pub fn quote_exact_input(
    pool: &V3PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
) -> Result<V3ExactInQuote, DexError> {
    pool.validate_static()?;
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "v3 adapter math".to_string(),
        }));
    }
    if pool.initialized_ticks.is_empty() {
        return Err(DexError::InvalidPool {
            reason: "initialized_ticks missing; exact V3 math unavailable".to_string(),
        });
    }

    let amount_in_after_fee = apply_fee_exact_in(amount_in, pool.fee_bps).map_err(DexError::MathError)?;
    if amount_in_after_fee.is_zero() {
        return Ok(V3ExactInQuote {
            amount_in,
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
            used_single_range_fallback: true,
        });
    }

    // If liquidityNet map is missing, allow fallback only for provably single-range swaps.
    if pool.tick_liquidity_net.is_empty() {
        let _next_tick = validate_single_range_fallback(pool, amount_in_after_fee, direction)?;
        let amount_out = math::calculate_v3_amount_out(
            amount_in,
            pool.sqrt_price_x96,
            pool.liquidity,
            pool.fee_bps,
            direction,
        )
        .map_err(DexError::MathError)?;
        let (sqrt_after, tick_after) = math::calculate_v3_post_swap_state(
            amount_in,
            pool.sqrt_price_x96,
            pool.liquidity,
            pool.fee_bps,
            direction,
        )
        .map_err(DexError::MathError)?;
        let execution = execution_price_wad(amount_in, amount_out, direction).map_err(DexError::MathError)?;
        let impact = math::calculate_v3_price_impact(pool.sqrt_price_x96, sqrt_after).map_err(DexError::MathError)?;
        return Ok(V3ExactInQuote {
            amount_in,
            amount_in_after_fee,
            amount_out,
            execution_price_wad: execution,
            price_impact_bps: impact,
            sqrt_price_before_x96: pool.sqrt_price_x96,
            sqrt_price_after_x96: sqrt_after,
            tick_before: pool.tick,
            tick_after,
            liquidity_before: pool.liquidity,
            liquidity_after: pool.liquidity,
            crossed_ticks: Vec::new(),
            used_single_range_fallback: true,
        });
    }

    let mut remaining_amount = amount_in;
    let mut amount_out_total = U256::ZERO;
    let mut current_sqrt = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut current_liquidity = pool.liquidity;
    let mut crossed_ticks = Vec::new();
    let zero_for_one = matches!(direction, SwapDirection::Token0ToToken1);

    let mut iterations = 0usize;
    while !remaining_amount.is_zero() {
        iterations = iterations.saturating_add(1);
        if iterations > 1024 {
            return Err(DexError::InvalidPool {
                reason: "swap iteration limit exceeded (possible malformed tick state)".to_string(),
            });
        }

        let next_tick = find_next_initialized_tick(
            current_tick,
            &pool.initialized_ticks,
            pool.tick_spacing,
            zero_for_one,
        )
        .map_err(DexError::MathError)?;
        let next_sqrt = math::get_sqrt_ratio_at_tick(next_tick).map_err(DexError::MathError)?;

        let max_amount_to_next = if zero_for_one {
            math::get_amount0_delta(next_sqrt, current_sqrt, current_liquidity, true)
                .map_err(DexError::MathError)?
        } else {
            math::get_amount1_delta(current_sqrt, next_sqrt, current_liquidity, true)
                .map_err(DexError::MathError)?
        };

        let segment_amount = remaining_amount.min(max_amount_to_next);
        let segment_fee = full_math::mul_div(segment_amount, U256::from(pool.fee_bps.as_u32()), BPS_DENOM)
            .map_err(|e| DexError::MathError(MathError::Overflow {
                operation: "quote_exact_input.segment_fee".to_string(),
                inputs: vec![],
                context: format!("mul_div failed: {}", e),
            }))?;
        let amount_after_fee = segment_amount
            .checked_sub(segment_fee)
            .ok_or_else(|| DexError::MathError(MathError::Underflow {
                operation: "quote_exact_input.segment_amount_after_fee".to_string(),
                inputs: vec![],
                context: "segment_amount - segment_fee".to_string(),
            }))?;

        let new_sqrt = if zero_for_one {
            sqrt_price_math::get_next_sqrt_price_from_amount_0_rounding_up(
                current_sqrt,
                current_liquidity,
                amount_after_fee,
                true,
            )
            .map_err(|e| DexError::MathError(MathError::InvalidInput {
                operation: "quote_exact_input.next_sqrt".to_string(),
                reason: format!("{}", e),
                context: "token0->token1".to_string(),
            }))?
        } else {
            sqrt_price_math::get_next_sqrt_price_from_amount_1_rounding_down(
                current_sqrt,
                current_liquidity,
                amount_after_fee,
                true,
            )
            .map_err(|e| DexError::MathError(MathError::InvalidInput {
                operation: "quote_exact_input.next_sqrt".to_string(),
                reason: format!("{}", e),
                context: "token1->token0".to_string(),
            }))?
        };

        let segment_out = if zero_for_one {
            math::get_amount1_delta(new_sqrt, current_sqrt, current_liquidity, false)
                .map_err(DexError::MathError)?
        } else {
            math::get_amount0_delta(current_sqrt, new_sqrt, current_liquidity, false)
                .map_err(DexError::MathError)?
        };

        amount_out_total = amount_out_total
            .checked_add(segment_out)
            .ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "quote_exact_input.amount_out_total".to_string(),
                inputs: vec![],
                context: "accumulate segment output".to_string(),
            }))?;
        remaining_amount = remaining_amount
            .checked_sub(segment_amount)
            .ok_or_else(|| DexError::MathError(MathError::Underflow {
                operation: "quote_exact_input.remaining_amount".to_string(),
                inputs: vec![],
                context: "remaining_amount - segment_amount".to_string(),
            }))?;
        current_sqrt = new_sqrt;
        current_tick = math::sqrt_price_to_tick(current_sqrt).map_err(DexError::MathError)?;

        // Apply liquidityNet iff this segment consumed to boundary and there is more input.
        if segment_amount == max_amount_to_next && !remaining_amount.is_zero() {
            let liq_net = pool.tick_liquidity_net.get(&next_tick).ok_or_else(|| DexError::InvalidPool {
                reason: format!("missing liquidityNet for crossed tick {}", next_tick),
            })?;
            let l = current_liquidity as i128;
            let new_l = if zero_for_one { l - *liq_net } else { l + *liq_net };
            if new_l < 0 {
                return Err(DexError::InvalidPool {
                    reason: format!("negative active liquidity after crossing tick {}", next_tick),
                });
            }
            current_liquidity = u128::try_from(new_l).map_err(|_| DexError::InvalidPool {
                reason: format!("active liquidity overflow after crossing tick {}", next_tick),
            })?;
            crossed_ticks.push(next_tick);
            if current_liquidity == 0 && !remaining_amount.is_zero() {
                return Err(DexError::InvalidPool {
                    reason: "active liquidity became zero before input was exhausted".to_string(),
                });
            }
        } else {
            break;
        }
    }

    let execution = execution_price_wad(amount_in, amount_out_total, direction).map_err(DexError::MathError)?;
    let impact = math::calculate_v3_price_impact(pool.sqrt_price_x96, current_sqrt).map_err(DexError::MathError)?;
    Ok(V3ExactInQuote {
        amount_in,
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
        used_single_range_fallback: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_when_initialized_ticks_missing() {
        let pool = V3PoolSnapshot {
            sqrt_price_x96: U256::from(79228162514264337593543950336u128),
            tick: 0,
            liquidity: 1_000_000_000_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![],
            tick_liquidity_net: HashMap::new(),
        };
        let err = quote_exact_input(&pool, U256::from(1_000_000_000_000_000_000u128), SwapDirection::Token0ToToken1)
            .unwrap_err();
        match err {
            DexError::InvalidPool { reason } => {
                assert!(reason.contains("initialized_ticks"));
            }
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn allows_single_range_fallback_when_proven_no_crossing() {
        let pool = V3PoolSnapshot {
            sqrt_price_x96: U256::from(79228162514264337593543950336u128),
            tick: 0,
            liquidity: 1_000_000_000_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60],
            tick_liquidity_net: HashMap::new(),
        };
        let q = quote_exact_input(&pool, U256::from(1_000_000_000u128), SwapDirection::Token0ToToken1).unwrap();
        assert!(q.used_single_range_fallback);
        assert!(q.amount_out > U256::ZERO);
        assert!(q.crossed_ticks.is_empty());
    }

    #[test]
    fn rejects_single_range_fallback_when_crossing_possible() {
        let pool = V3PoolSnapshot {
            sqrt_price_x96: U256::from(79228162514264337593543950336u128),
            tick: 0,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60],
            tick_liquidity_net: HashMap::new(),
        };
        let err = quote_exact_input(&pool, U256::from(1_000_000_000_000_000_000u128), SwapDirection::Token0ToToken1)
            .unwrap_err();
        match err {
            DexError::InvalidPool { reason } => {
                assert!(reason.contains("fallback rejected"));
            }
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn crosses_ticks_when_liquidity_net_available() {
        let mut liq = HashMap::new();
        liq.insert(60, 500_000_000i128);
        liq.insert(120, 0i128);
        let sqrt_59 = math::get_sqrt_ratio_at_tick(59).unwrap();
        let sqrt_60 = math::get_sqrt_ratio_at_tick(60).unwrap();
        let max_to_60 = math::get_amount1_delta(sqrt_59, sqrt_60, 1_000_000_000u128, true).unwrap();
        let pool = V3PoolSnapshot {
            sqrt_price_x96: sqrt_59,
            tick: 59,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![0, 60, 120],
            tick_liquidity_net: liq,
        };
        let q = quote_exact_input(&pool, max_to_60 * U256::from(2u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(!q.used_single_range_fallback);
        assert!(q.amount_out > U256::ZERO);
        assert!(!q.crossed_ticks.is_empty());
    }

    #[test]
    fn fails_closed_if_crossed_tick_missing_liquidity_net() {
        let mut liq = HashMap::new();
        // No entry for tick 60 even though it can be crossed.
        liq.insert(120, 1);
        let pool = V3PoolSnapshot {
            sqrt_price_x96: U256::from(79228162514264337593543950336u128),
            tick: 0,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60, 120],
            tick_liquidity_net: liq,
        };
        let err = quote_exact_input(&pool, U256::from(2_000_000_000_000_000_000u128), SwapDirection::Token1ToToken0)
            .unwrap_err();
        match err {
            DexError::InvalidPool { reason } => {
                assert!(reason.contains("missing liquidityNet"));
            }
            _ => panic!("expected InvalidPool"),
        }
    }
}

