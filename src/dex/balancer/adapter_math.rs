//! Decoupled, production-grade Balancer weighted-pool exact-in quoting adapter.
//!
//! Pure deterministic math module with fail-closed behavior and post-state outputs.

use alloy_primitives::U256;
use primitive_types::U256 as PrimU256;
use uniswap_v3_math::full_math;

use crate::core::{BasisPoints, DexError, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::balancer::conversions::{swap_fee_bps_to_18_decimal, to_alloy_u256, to_primitive_u256};
use crate::dex::balancer::math;
use crate::dex::common::ethers_to_alloy;

const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);

/// Serializable weighted-pool snapshot for deterministic quoting.
#[derive(Debug, Clone)]
pub struct BalancerPoolSnapshot {
    pub balance0: U256,
    pub balance1: U256,
    pub weight0: U256, // 1e18 scale
    pub weight1: U256, // 1e18 scale
    pub swap_fee_bps: BasisPoints,
}

impl From<&crate::data::pool_state::BalancerPoolState> for BalancerPoolSnapshot {
    fn from(v: &crate::data::pool_state::BalancerPoolState) -> Self {
        // Weighted adapter module currently supports 2-token projections.
        let (b0, b1) = if v.balances.len() >= 2 {
            (ethers_to_alloy(v.balances[0]), ethers_to_alloy(v.balances[1]))
        } else {
            (U256::ZERO, U256::ZERO)
        };
        let (w0, w1) = if v.weights.len() >= 2 {
            (ethers_to_alloy(v.weights[0]), ethers_to_alloy(v.weights[1]))
        } else {
            (U256::ZERO, U256::ZERO)
        };
        Self {
            balance0: b0,
            balance1: b1,
            weight0: w0,
            weight1: w1,
            swap_fee_bps: BasisPoints::new_const(v.swap_fee_bps),
        }
    }
}

/// Exact-in quote result with post-trade balances.
#[derive(Debug, Clone)]
pub struct BalancerExactInQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub execution_price_wad: U256,
    pub price_impact_bps: u32,
    pub balance0_before: U256,
    pub balance1_before: U256,
    pub balance0_after: U256,
    pub balance1_after: U256,
}

#[inline(always)]
fn execution_price_wad(amount_in: U256, amount_out: U256, direction: SwapDirection) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "balancer.execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    match direction {
        SwapDirection::Token0ToToken1 => {
            full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
                operation: "balancer.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if amount_out.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "balancer.execution_price_wad".to_string(),
                    context: "amount_out".to_string(),
                });
            }
            full_math::mul_div(amount_in, WAD, amount_out).map_err(|e| MathError::Overflow {
                operation: "balancer.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("inverse mul_div failed: {}", e),
            })
        }
    }
}

#[inline(always)]
fn price_impact_bps(before_wad: U256, after_wad: U256) -> Result<u32, MathError> {
    if before_wad.is_zero() {
        return Ok(0);
    }
    let diff = if after_wad >= before_wad {
        after_wad - before_wad
    } else {
        before_wad - after_wad
    };
    let impact = full_math::mul_div(diff, BPS_DENOM, before_wad).map_err(|e| MathError::Overflow {
        operation: "balancer.price_impact_bps".to_string(),
        inputs: vec![],
        context: format!("mul_div failed: {}", e),
    })?;
    Ok(if impact > BPS_DENOM {
        10_000
    } else {
        impact.as_limbs()[0] as u32
    })
}

/// Deterministic exact-input quote for 2-token weighted pools.
pub fn quote_exact_input(
    pool: &BalancerPoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
) -> Result<BalancerExactInQuote, DexError> {
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "balancer.quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "".to_string(),
        }));
    }
    if pool.swap_fee_bps.as_u32() >= 10_000 {
        return Err(DexError::InvalidPool {
            reason: format!("invalid swap_fee_bps {}; must be < 10000", pool.swap_fee_bps.as_u32()),
        });
    }

    let (balance_in, balance_out, weight_in, weight_out): (PrimU256, PrimU256, PrimU256, PrimU256) = match direction {
        SwapDirection::Token0ToToken1 => (
            to_primitive_u256(pool.balance0),
            to_primitive_u256(pool.balance1),
            to_primitive_u256(pool.weight0),
            to_primitive_u256(pool.weight1),
        ),
        SwapDirection::Token1ToToken0 => (
            to_primitive_u256(pool.balance1),
            to_primitive_u256(pool.balance0),
            to_primitive_u256(pool.weight1),
            to_primitive_u256(pool.weight0),
        ),
    };

    let swap_fee_18 = swap_fee_bps_to_18_decimal(pool.swap_fee_bps.as_u32());
    let amount_out_prim = math::calculate_swap_output(
        to_primitive_u256(amount_in),
        balance_in,
        balance_out,
        weight_in,
        weight_out,
        swap_fee_18,
    )
    .map_err(DexError::MathError)?;
    let amount_out = to_alloy_u256(amount_out_prim);
    if amount_out.is_zero() {
        return Err(DexError::InvalidPool {
            reason: "amount_out computed as zero".to_string(),
        });
    }

    let (balance0_after, balance1_after) = match direction {
        SwapDirection::Token0ToToken1 => {
            let b0 = pool.balance0
                .checked_add(amount_in)
                .ok_or_else(|| DexError::MathError(MathError::Overflow {
                    operation: "balancer.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "balance0 + amount_in".to_string(),
                }))?;
            let b1 = pool.balance1
                .checked_sub(amount_out)
                .ok_or_else(|| DexError::MathError(MathError::Underflow {
                    operation: "balancer.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "balance1 - amount_out".to_string(),
                }))?;
            (b0, b1)
        }
        SwapDirection::Token1ToToken0 => {
            let b1 = pool.balance1
                .checked_add(amount_in)
                .ok_or_else(|| DexError::MathError(MathError::Overflow {
                    operation: "balancer.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "balance1 + amount_in".to_string(),
                }))?;
            let b0 = pool.balance0
                .checked_sub(amount_out)
                .ok_or_else(|| DexError::MathError(MathError::Underflow {
                    operation: "balancer.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "balance0 - amount_out".to_string(),
                }))?;
            (b0, b1)
        }
    };

    let spot_before = match direction {
        SwapDirection::Token0ToToken1 => math::calculate_balancer_price(
            to_primitive_u256(pool.balance0),
            to_primitive_u256(pool.balance1),
            to_primitive_u256(pool.weight0),
            to_primitive_u256(pool.weight1),
        ),
        SwapDirection::Token1ToToken0 => math::calculate_balancer_price(
            to_primitive_u256(pool.balance1),
            to_primitive_u256(pool.balance0),
            to_primitive_u256(pool.weight1),
            to_primitive_u256(pool.weight0),
        ),
    }
    .map(to_alloy_u256)
    .map_err(DexError::MathError)?;

    let spot_after = match direction {
        SwapDirection::Token0ToToken1 => math::calculate_balancer_price(
            to_primitive_u256(balance0_after),
            to_primitive_u256(balance1_after),
            to_primitive_u256(pool.weight0),
            to_primitive_u256(pool.weight1),
        ),
        SwapDirection::Token1ToToken0 => math::calculate_balancer_price(
            to_primitive_u256(balance1_after),
            to_primitive_u256(balance0_after),
            to_primitive_u256(pool.weight1),
            to_primitive_u256(pool.weight0),
        ),
    }
    .map(to_alloy_u256)
    .map_err(DexError::MathError)?;

    let execution = execution_price_wad(amount_in, amount_out, direction).map_err(DexError::MathError)?;
    let impact_bps = price_impact_bps(spot_before, spot_after).map_err(DexError::MathError)?;

    Ok(BalancerExactInQuote {
        amount_in,
        amount_out,
        execution_price_wad: execution,
        price_impact_bps: impact_bps,
        balance0_before: pool.balance0,
        balance1_before: pool.balance1,
        balance0_after,
        balance1_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(v: u64) -> U256 {
        // convert percent-like integer to 1e18-scale decimal (e.g., 50 -> 0.5e18)
        U256::from(v) * U256::from(10u64).pow(U256::from(16u64))
    }

    #[test]
    fn quote_exact_input_token0_to_token1() {
        let pool = BalancerPoolSnapshot {
            balance0: U256::from(1_000_000_000u64),
            balance1: U256::from(2_000_000_000u64),
            weight0: w(50),
            weight1: w(50),
            swap_fee_bps: BasisPoints::new_const(30),
        };
        let q = quote_exact_input(&pool, U256::from(10_000u64), SwapDirection::Token0ToToken1).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert!(q.balance0_after > q.balance0_before);
        assert!(q.balance1_after < q.balance1_before);
    }

    #[test]
    fn quote_exact_input_token1_to_token0() {
        let pool = BalancerPoolSnapshot {
            balance0: U256::from(2_000_000_000u64),
            balance1: U256::from(1_000_000_000u64),
            weight0: w(70),
            weight1: w(30),
            swap_fee_bps: BasisPoints::new_const(30),
        };
        let q = quote_exact_input(&pool, U256::from(10_000u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert!(q.balance1_after > q.balance1_before);
        assert!(q.balance0_after < q.balance0_before);
    }

    #[test]
    fn quote_exact_input_rejects_invalid_fee() {
        let pool = BalancerPoolSnapshot {
            balance0: U256::from(1_000u64),
            balance1: U256::from(1_000u64),
            weight0: w(50),
            weight1: w(50),
            swap_fee_bps: BasisPoints::new_const(10_000),
        };
        let err = quote_exact_input(&pool, U256::from(1u64), SwapDirection::Token0ToToken1).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("invalid swap_fee_bps")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn quote_exact_input_rejects_zero_amount() {
        let pool = BalancerPoolSnapshot {
            balance0: U256::from(1_000u64),
            balance1: U256::from(1_000u64),
            weight0: w(50),
            weight1: w(50),
            swap_fee_bps: BasisPoints::new_const(30),
        };
        let err = quote_exact_input(&pool, U256::ZERO, SwapDirection::Token0ToToken1).unwrap_err();
        match err {
            DexError::MathError(MathError::InvalidInput { .. }) => {}
            _ => panic!("expected MathError::InvalidInput"),
        }
    }
}

