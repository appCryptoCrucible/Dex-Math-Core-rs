//! Decoupled, production-grade Uniswap V2 exact-in quoting adapter.
//!
//! Pure deterministic math module with fail-closed behavior.

use alloy_primitives::U256;
use uniswap_v3_math::full_math;

use crate::core::{BasisPoints, DexError, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::common::ethers_to_alloy;
use crate::dex::uniswap_v2::math;

const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);

/// Serializable V2 pool snapshot independent of runtime/state manager plumbing.
#[derive(Debug, Clone)]
pub struct V2PoolSnapshot {
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: BasisPoints,
}

impl From<&crate::data::pool_state::V2PoolState> for V2PoolSnapshot {
    fn from(v: &crate::data::pool_state::V2PoolState) -> Self {
        Self {
            reserve0: ethers_to_alloy(v.reserve0),
            reserve1: ethers_to_alloy(v.reserve1),
            // V2 standard fee for this crate.
            fee_bps: BasisPoints::new_const(30),
        }
    }
}

/// Exact-in quote result with post-trade reserves.
#[derive(Debug, Clone)]
pub struct V2ExactInQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub execution_price_wad: U256,
    pub price_impact_bps: u32,
    pub reserve0_before: U256,
    pub reserve1_before: U256,
    pub reserve0_after: U256,
    pub reserve1_after: U256,
}

#[inline(always)]
fn execution_price_wad(amount_in: U256, amount_out: U256, direction: SwapDirection) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "v2.execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    match direction {
        SwapDirection::Token0ToToken1 => {
            full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
                operation: "v2.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if amount_out.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "v2.execution_price_wad".to_string(),
                    context: "amount_out".to_string(),
                });
            }
            full_math::mul_div(amount_in, WAD, amount_out).map_err(|e| MathError::Overflow {
                operation: "v2.execution_price_wad".to_string(),
                inputs: vec![],
                context: format!("inverse mul_div failed: {}", e),
            })
        }
    }
}

#[inline(always)]
fn spot_price_wad(reserve0: U256, reserve1: U256, direction: SwapDirection) -> Result<U256, MathError> {
    match direction {
        SwapDirection::Token0ToToken1 => {
            if reserve0.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "v2.spot_price_wad".to_string(),
                    context: "reserve0".to_string(),
                });
            }
            full_math::mul_div(reserve1, WAD, reserve0).map_err(|e| MathError::Overflow {
                operation: "v2.spot_price_wad".to_string(),
                inputs: vec![],
                context: format!("token0->token1 mul_div failed: {}", e),
            })
        }
        SwapDirection::Token1ToToken0 => {
            if reserve1.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "v2.spot_price_wad".to_string(),
                    context: "reserve1".to_string(),
                });
            }
            full_math::mul_div(reserve0, WAD, reserve1).map_err(|e| MathError::Overflow {
                operation: "v2.spot_price_wad".to_string(),
                inputs: vec![],
                context: format!("token1->token0 mul_div failed: {}", e),
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
        operation: "v2.price_impact_bps".to_string(),
        inputs: vec![],
        context: format!("mul_div failed: {}", e),
    })?;
    Ok(if impact > BPS_DENOM {
        10_000
    } else {
        impact.as_limbs()[0] as u32
    })
}

/// Deterministic exact-input quote with fail-closed validation.
pub fn quote_exact_input(
    pool: &V2PoolSnapshot,
    amount_in: U256,
    direction: SwapDirection,
) -> Result<V2ExactInQuote, DexError> {
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "v2.quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "".to_string(),
        }));
    }
    if pool.reserve0.is_zero() || pool.reserve1.is_zero() {
        return Err(DexError::InvalidPool {
            reason: format!(
                "invalid reserves: reserve0={}, reserve1={}",
                pool.reserve0, pool.reserve1
            ),
        });
    }
    if pool.fee_bps.as_u32() >= 10_000 {
        return Err(DexError::InvalidPool {
            reason: format!("invalid fee_bps {}; must be < 10000", pool.fee_bps.as_u32()),
        });
    }

    let (reserve_in, reserve_out) = match direction {
        SwapDirection::Token0ToToken1 => (pool.reserve0, pool.reserve1),
        SwapDirection::Token1ToToken0 => (pool.reserve1, pool.reserve0),
    };
    let amount_out = math::calculate_v2_amount_out(amount_in, reserve_in, reserve_out, pool.fee_bps)
        .map_err(DexError::MathError)?;
    if amount_out.is_zero() {
        return Err(DexError::InvalidPool {
            reason: "amount_out computed as zero".to_string(),
        });
    }
    if amount_out >= reserve_out {
        return Err(DexError::InvalidPool {
            reason: format!(
                "amount_out {} invalid vs reserve_out {}",
                amount_out, reserve_out
            ),
        });
    }

    let (reserve0_after, reserve1_after) = match direction {
        SwapDirection::Token0ToToken1 => {
            let r0 = pool.reserve0
                .checked_add(amount_in)
                .ok_or_else(|| DexError::MathError(MathError::Overflow {
                    operation: "v2.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "reserve0 + amount_in".to_string(),
                }))?;
            let r1 = pool.reserve1
                .checked_sub(amount_out)
                .ok_or_else(|| DexError::MathError(MathError::Underflow {
                    operation: "v2.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "reserve1 - amount_out".to_string(),
                }))?;
            (r0, r1)
        }
        SwapDirection::Token1ToToken0 => {
            let r1 = pool.reserve1
                .checked_add(amount_in)
                .ok_or_else(|| DexError::MathError(MathError::Overflow {
                    operation: "v2.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "reserve1 + amount_in".to_string(),
                }))?;
            let r0 = pool.reserve0
                .checked_sub(amount_out)
                .ok_or_else(|| DexError::MathError(MathError::Underflow {
                    operation: "v2.quote_exact_input".to_string(),
                    inputs: vec![],
                    context: "reserve0 - amount_out".to_string(),
                }))?;
            (r0, r1)
        }
    };

    let execution = execution_price_wad(amount_in, amount_out, direction).map_err(DexError::MathError)?;
    let spot_before = spot_price_wad(pool.reserve0, pool.reserve1, direction).map_err(DexError::MathError)?;
    let spot_after = spot_price_wad(reserve0_after, reserve1_after, direction).map_err(DexError::MathError)?;
    let impact_bps = price_impact_bps(spot_before, spot_after).map_err(DexError::MathError)?;

    Ok(V2ExactInQuote {
        amount_in,
        amount_out,
        execution_price_wad: execution,
        price_impact_bps: impact_bps,
        reserve0_before: pool.reserve0,
        reserve1_before: pool.reserve1,
        reserve0_after,
        reserve1_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_exact_input_token0_to_token1_updates_reserves() {
        let pool = V2PoolSnapshot {
            reserve0: U256::from(100_000_000u64),
            reserve1: U256::from(200_000_000u64),
            fee_bps: BasisPoints::new_const(30),
        };
        let q = quote_exact_input(&pool, U256::from(1_000_000u64), SwapDirection::Token0ToToken1).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert!(q.reserve0_after > q.reserve0_before);
        assert!(q.reserve1_after < q.reserve1_before);
        assert!(q.execution_price_wad > U256::ZERO);
    }

    #[test]
    fn quote_exact_input_token1_to_token0_updates_reserves() {
        let pool = V2PoolSnapshot {
            reserve0: U256::from(200_000_000u64),
            reserve1: U256::from(100_000_000u64),
            fee_bps: BasisPoints::new_const(30),
        };
        let q = quote_exact_input(&pool, U256::from(1_000_000u64), SwapDirection::Token1ToToken0).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert!(q.reserve1_after > q.reserve1_before);
        assert!(q.reserve0_after < q.reserve0_before);
        assert!(q.execution_price_wad > U256::ZERO);
    }

    #[test]
    fn quote_exact_input_rejects_invalid_fee() {
        let pool = V2PoolSnapshot {
            reserve0: U256::from(1000u64),
            reserve1: U256::from(1000u64),
            fee_bps: BasisPoints::new_const(10_000),
        };
        let err = quote_exact_input(&pool, U256::from(1u64), SwapDirection::Token0ToToken1).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("invalid fee_bps")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn quote_exact_input_rejects_zero_amount() {
        let pool = V2PoolSnapshot {
            reserve0: U256::from(1000u64),
            reserve1: U256::from(1000u64),
            fee_bps: BasisPoints::new_const(30),
        };
        let err = quote_exact_input(&pool, U256::ZERO, SwapDirection::Token0ToToken1).unwrap_err();
        match err {
            DexError::MathError(MathError::InvalidInput { .. }) => {}
            _ => panic!("expected MathError::InvalidInput"),
        }
    }
}

