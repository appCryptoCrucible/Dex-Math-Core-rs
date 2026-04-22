//! Decoupled, production-grade Kyber Elastic exact-in quoting adapter.
//!
//! Runtime-agnostic deterministic quoting with strict fail-closed behavior.

use std::collections::HashMap;

use alloy_primitives::U256;
use uniswap_v3_math::full_math;

use crate::core::{BasisPoints, DexError, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::kyber::math::{swap_math, tick_math};
use crate::dex::uniswap_v3;

const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

/// Serializable Kyber pool snapshot for deterministic adapter math.
#[derive(Debug, Clone)]
pub struct KyberPoolSnapshot {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee_bps: BasisPoints,
    pub tick_spacing: i32,
    pub initialized_ticks: Vec<i32>,
    pub tick_liquidity_net: HashMap<i32, i128>,
}

impl KyberPoolSnapshot {
    fn validate_static(&self) -> Result<(), DexError> {
        if self.sqrt_price_x96.is_zero() {
            return Err(DexError::InvalidPool {
                reason: "sqrt_price_x96 cannot be zero".to_string(),
            });
        }
        tick_math::get_tick_at_sqrt_ratio(self.sqrt_price_x96).map_err(DexError::MathError)?;
        if self.liquidity == 0 {
            return Err(DexError::InvalidPool {
                reason: "liquidity cannot be zero".to_string(),
            });
        }
        if self.fee_bps.as_u32() >= 10_000 {
            return Err(DexError::InvalidPool {
                reason: format!("fee_bps must be <10000, got {}", self.fee_bps.as_u32()),
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
        Ok(())
    }
}

impl TryFrom<&crate::data::kyber_pool_state::KyberPoolState> for KyberPoolSnapshot {
    type Error = DexError;

    fn try_from(v: &crate::data::kyber_pool_state::KyberPoolState) -> Result<Self, Self::Error> {
        let mut tick_liquidity_net = HashMap::with_capacity(v.tick_liquidity.len());
        for (k, liq) in &v.tick_liquidity {
            let signed = i128::try_from(*liq).map_err(|_| DexError::InvalidPool {
                reason: format!("tick_liquidity[{}] exceeds i128 range", k),
            })?;
            tick_liquidity_net.insert(*k, signed);
        }
        let mut initialized_ticks: Vec<i32> = v.initialized_ticks.iter().copied().collect();
        initialized_ticks.sort_unstable();
        Ok(Self {
            sqrt_price_x96: crate::dex::common::ethers_to_alloy(v.sqrt_price_x96),
            tick: v.current_tick,
            liquidity: v.liquidity,
            fee_bps: BasisPoints::new_const(v.fee_tier),
            tick_spacing: v.tick_spacing,
            initialized_ticks,
            tick_liquidity_net,
        })
    }
}

/// Exact-input quote result with post-state and diagnostics.
#[derive(Debug, Clone)]
pub struct KyberExactInQuote {
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
fn execution_price_wad(amount_in: U256, amount_out: U256, direction: SwapDirection) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "kyber.execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    match direction {
        SwapDirection::Token0ToToken1 => {
            full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
                operation: "kyber.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if amount_out.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "kyber.execution_price_wad".to_string(),
                    context: "amount_out".to_string(),
                });
            }
            full_math::mul_div(amount_in, WAD, amount_out).map_err(|e| MathError::Overflow {
                operation: "kyber.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("inverse mul_div failed: {}", e),
            })
        }
    }
}

#[inline(always)]
fn apply_fee(amount_in: U256, fee_bps: BasisPoints) -> Result<U256, MathError> {
    let multiplier = U256::from(10_000u32 - fee_bps.as_u32());
    amount_in
        .checked_mul(multiplier)
        .and_then(|v| v.checked_div(BPS_DENOM))
        .ok_or_else(|| MathError::Overflow {
            operation: "kyber.apply_fee".to_string(),
            inputs: vec![],
            context: format!("amount_in={}, fee_bps={}", amount_in, fee_bps.as_u32()),
        })
}

fn find_next_initialized_tick(
    current_tick: i32,
    initialized_ticks: &[i32],
    tick_spacing: i32,
    zero_for_one: bool,
) -> Result<i32, MathError> {
    if tick_spacing <= 0 {
        return Err(MathError::InvalidInput {
            operation: "kyber.find_next_initialized_tick".to_string(),
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
    pool: &KyberPoolSnapshot,
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
    let next_sqrt = tick_math::get_sqrt_ratio_at_tick(next_tick).map_err(DexError::MathError)?;
    let max_to_next = if zero_for_one {
        uniswap_v3::math::get_amount0_delta(next_sqrt, pool.sqrt_price_x96, pool.liquidity, true)
            .map_err(DexError::MathError)?
    } else {
        uniswap_v3::math::get_amount1_delta(pool.sqrt_price_x96, next_sqrt, pool.liquidity, true)
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

/// Deterministic exact-input quote with tick crossing and strict fail-closed checks.
pub fn quote_exact_input(
    pool: &KyberPoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
) -> Result<KyberExactInQuote, DexError> {
    pool.validate_static()?;
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "kyber.quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "".to_string(),
        }));
    }
    if pool.initialized_ticks.is_empty() {
        return Err(DexError::InvalidPool {
            reason: "initialized_ticks missing; exact Kyber quote unavailable".to_string(),
        });
    }

    let amount_in_after_fee = apply_fee(amount_in, pool.fee_bps).map_err(DexError::MathError)?;
    if amount_in_after_fee.is_zero() {
        return Ok(KyberExactInQuote {
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

    let zero_for_one = matches!(direction, SwapDirection::Token0ToToken1);

    // Missing liquidityNet map: only allow if exact no-crossing proof holds.
    if pool.tick_liquidity_net.is_empty() {
        let next_tick = validate_single_range_fallback(pool, amount_in_after_fee, direction)?;
        let target = tick_math::get_sqrt_ratio_at_tick(next_tick).map_err(DexError::MathError)?;
        let specified_i128 = i128::try_from(amount_in).map_err(|_| DexError::InvalidPool {
            reason: "amount_in exceeds i128 range required by swap step".to_string(),
        })?;
        let step = swap_math::compute_swap_step(
            pool.liquidity,
            pool.sqrt_price_x96,
            target,
            pool.fee_bps.as_u32(),
            specified_i128,
            true,
            zero_for_one,
        )
        .map_err(DexError::MathError)?;
        if step.used_amount <= 0 {
            return Err(DexError::InvalidPool {
                reason: "swap step produced non-positive used_amount".to_string(),
            });
        }
        if step.returned_amount >= 0 {
            return Err(DexError::InvalidPool {
                reason: "swap step produced non-negative returned_amount for exact-input".to_string(),
            });
        }
        let amount_out = U256::from((-step.returned_amount) as u128);
        let tick_after = tick_math::get_tick_at_sqrt_ratio(step.next_sqrt_p).map_err(DexError::MathError)?;
        let execution = execution_price_wad(amount_in, amount_out, direction).map_err(DexError::MathError)?;
        let impact = uniswap_v3::math::calculate_v3_price_impact(pool.sqrt_price_x96, step.next_sqrt_p)
            .map_err(DexError::MathError)?;
        return Ok(KyberExactInQuote {
            amount_in,
            amount_in_after_fee,
            amount_out,
            execution_price_wad: execution,
            price_impact_bps: impact,
            sqrt_price_before_x96: pool.sqrt_price_x96,
            sqrt_price_after_x96: step.next_sqrt_p,
            tick_before: pool.tick,
            tick_after,
            liquidity_before: pool.liquidity,
            liquidity_after: pool.liquidity,
            crossed_ticks: Vec::new(),
            used_single_range_fallback: true,
        });
    }

    let mut remaining = amount_in;
    let mut amount_out_total = U256::ZERO;
    let mut current_sqrt = pool.sqrt_price_x96;
    let mut current_tick = pool.tick;
    let mut current_liquidity = pool.liquidity;
    let mut crossed_ticks = Vec::new();

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
        let target = tick_math::get_sqrt_ratio_at_tick(next_tick).map_err(DexError::MathError)?;
        let specified_i128 = i128::try_from(remaining).map_err(|_| DexError::InvalidPool {
            reason: "remaining amount exceeds i128 range required by swap step".to_string(),
        })?;
        let step = swap_math::compute_swap_step(
            current_liquidity,
            current_sqrt,
            target,
            pool.fee_bps.as_u32(),
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
        let out = U256::from((-step.returned_amount) as u128);

        amount_out_total = amount_out_total
            .checked_add(out)
            .ok_or_else(|| DexError::MathError(MathError::Overflow {
                operation: "kyber.quote_exact_input.amount_out_total".to_string(),
                inputs: vec![],
                context: "accumulate output".to_string(),
            }))?;
        remaining = remaining
            .checked_sub(used)
            .ok_or_else(|| DexError::MathError(MathError::Underflow {
                operation: "kyber.quote_exact_input.remaining".to_string(),
                inputs: vec![],
                context: "remaining - used".to_string(),
            }))?;
        current_sqrt = step.next_sqrt_p;
        current_tick = tick_math::get_tick_at_sqrt_ratio(current_sqrt).map_err(DexError::MathError)?;

        if step.next_sqrt_p == target && !remaining.is_zero() {
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
                reason: format!("liquidity overflow after crossing tick {}", next_tick),
            })?;
            crossed_ticks.push(next_tick);
            if current_liquidity == 0 && !remaining.is_zero() {
                return Err(DexError::InvalidPool {
                    reason: "liquidity became zero before amount was exhausted".to_string(),
                });
            }
        } else {
            break;
        }
    }

    let execution = execution_price_wad(amount_in, amount_out_total, direction).map_err(DexError::MathError)?;
    let impact = uniswap_v3::math::calculate_v3_price_impact(pool.sqrt_price_x96, current_sqrt)
        .map_err(DexError::MathError)?;

    Ok(KyberExactInQuote {
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

    fn base_snapshot() -> KyberPoolSnapshot {
        KyberPoolSnapshot {
            sqrt_price_x96: U256::from(79228162514264337593543950336u128), // tick 0
            tick: 0,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(25),
            tick_spacing: 60,
            initialized_ticks: vec![-60, 0, 60, 120],
            tick_liquidity_net: HashMap::new(),
        }
    }

    #[test]
    fn rejects_missing_initialized_ticks() {
        let mut s = base_snapshot();
        s.initialized_ticks.clear();
        let err = quote_exact_input(&s, U256::from(1000u64), SwapDirection::Token0ToToken1).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("initialized_ticks")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn allows_fallback_when_no_crossing_proven() {
        let s = base_snapshot();
        let q = quote_exact_input(&s, U256::from(1_000u64), SwapDirection::Token0ToToken1).unwrap();
        assert!(q.used_single_range_fallback);
        assert!(q.amount_out > U256::ZERO);
    }

    #[test]
    fn rejects_fallback_when_crossing_possible() {
        let s = base_snapshot();
        let err = quote_exact_input(&s, U256::from(100_000_000_000_000_000u128), SwapDirection::Token0ToToken1)
            .unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("fallback rejected")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn crosses_ticks_when_liquidity_map_available() {
        let mut s = base_snapshot();
        s.tick_liquidity_net.insert(60, 100_000_000);
        s.tick_liquidity_net.insert(120, 0);
        s.tick_liquidity_net.insert(180, 0);
        // Start just below first initialized up-tick so crossing is deterministic.
        s.tick = 59;
        s.sqrt_price_x96 = tick_math::get_sqrt_ratio_at_tick(59).unwrap();
        let sqrt_60 = tick_math::get_sqrt_ratio_at_tick(60).unwrap();
        let max_to_60 =
            crate::dex::uniswap_v3::math::get_amount1_delta(s.sqrt_price_x96, sqrt_60, s.liquidity, true).unwrap();
        let q = quote_exact_input(&s, max_to_60 * U256::from(2u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(!q.used_single_range_fallback);
        assert!(!q.crossed_ticks.is_empty());
        assert!(q.amount_out > U256::ZERO);
    }
}

