//! Decoupled, production-grade Curve StableSwap exact-in quoting adapter.
//!
//! This module is runtime-agnostic and deterministic: it depends only on an
//! explicit pool snapshot and returns exact quote + post-state outputs.

use alloy_primitives::U256;
use uniswap_v3_math::full_math;

use crate::core::{DexError, MathError};
use crate::data::curve_registry::StableswapMathVariant;
use crate::dex::curve::math;

const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);

/// Serializable StableSwap snapshot for deterministic quoting.
#[derive(Debug, Clone)]
pub struct CurvePoolSnapshot {
    pub balances: Vec<U256>,
    pub decimals: Vec<u8>,
    pub stored_rates: Option<Vec<U256>>,
    pub variant: StableswapMathVariant,
    pub amplification: U256,
    pub fee_raw: U256,
    pub fee_bps: u32,
}

impl From<&crate::data::pool_state::CurvePoolState> for CurvePoolSnapshot {
    fn from(v: &crate::data::pool_state::CurvePoolState) -> Self {
        Self {
            balances: v.balances.iter().map(|x| crate::dex::common::ethers_to_alloy(*x)).collect(),
            decimals: v.decimals.clone(),
            stored_rates: v
                .stableswap_stored_rates
                .as_ref()
                .map(|r| r.iter().map(|x| crate::dex::common::ethers_to_alloy(*x)).collect()),
            variant: v.stableswap_math_variant,
            amplification: crate::dex::common::ethers_to_alloy(v.amplification),
            fee_raw: crate::dex::common::ethers_to_alloy(v.fee_raw),
            fee_bps: v.fee_bps,
        }
    }
}

/// Exact-input quote result with deterministic post-swap balances.
#[derive(Debug, Clone)]
pub struct CurveExactInQuote {
    pub amount_in: U256,
    pub amount_out: U256,
    pub execution_price_wad: U256,
    pub price_impact_bps: u32,
    pub token_in_index: usize,
    pub token_out_index: usize,
    pub balances_before: Vec<U256>,
    pub balances_after: Vec<U256>,
}

#[inline(always)]
fn execution_price_wad(amount_in: U256, amount_out: U256) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "curve.execution_price_wad".to_string(),
            context: "amount_in".to_string(),
        });
    }
    full_math::mul_div(amount_out, WAD, amount_in).map_err(|e| MathError::Overflow {
        operation: "curve.execution_price_wad".to_string(),
        inputs: vec![],
        context: format!("mul_div failed: {}", e),
    })
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
        operation: "curve.price_impact_bps".to_string(),
        inputs: vec![],
        context: format!("mul_div failed: {}", e),
    })?;
    Ok(if impact > BPS_DENOM {
        10_000
    } else {
        impact.as_limbs()[0] as u32
    })
}

fn validate_snapshot(
    pool: &CurvePoolSnapshot,
    token_in_index: usize,
    token_out_index: usize,
) -> Result<(), DexError> {
    if pool.balances.len() < 2 {
        return Err(DexError::InvalidPool {
            reason: format!("curve pool must have >=2 balances, got {}", pool.balances.len()),
        });
    }
    if pool.decimals.len() != pool.balances.len() {
        return Err(DexError::InvalidPool {
            reason: format!(
                "decimals length {} must equal balances length {}",
                pool.decimals.len(),
                pool.balances.len()
            ),
        });
    }
    if token_in_index >= pool.balances.len() || token_out_index >= pool.balances.len() {
        return Err(DexError::InvalidPool {
            reason: format!(
                "token indices out of bounds: in={}, out={}, len={}",
                token_in_index,
                token_out_index,
                pool.balances.len()
            ),
        });
    }
    if token_in_index == token_out_index {
        return Err(DexError::InvalidPool {
            reason: "token_in_index and token_out_index must differ".to_string(),
        });
    }
    if pool.amplification.is_zero() {
        return Err(DexError::InvalidPool {
            reason: "amplification cannot be zero".to_string(),
        });
    }
    if pool.fee_bps > 10_000 {
        return Err(DexError::InvalidPool {
            reason: format!("fee_bps {} > 10000", pool.fee_bps),
        });
    }
    Ok(())
}

/// Deterministic exact-input quote using StableSwap math.
pub fn quote_exact_input(
    pool: &CurvePoolSnapshot,
    token_in_index: usize,
    token_out_index: usize,
    amount_in: U256,
) -> Result<CurveExactInQuote, DexError> {
    validate_snapshot(pool, token_in_index, token_out_index)?;
    if amount_in.is_zero() {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "curve.quote_exact_input".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: "".to_string(),
        }));
    }

    let owned_rates;
    let rates: &[U256] = if let Some(stored) = pool.stored_rates.as_deref() {
        stored
    } else {
        owned_rates = math::stableswap_rates_resolve(&pool.decimals, None).map_err(DexError::MathError)?;
        &owned_rates
    };
    let xp_before = math::stableswap_xp_from_rates(&pool.balances, &rates).map_err(DexError::MathError)?;

    let spot_before = math::calculate_curve_price(
        token_in_index,
        token_out_index,
        &xp_before,
        &rates,
        pool.variant,
        pool.amplification,
    )
    .map_err(DexError::MathError)?;

    let amount_out = math::calculate_swap_output_from_xp(
        amount_in,
        token_in_index,
        token_out_index,
        &xp_before,
        &rates,
        pool.variant,
        pool.amplification,
        pool.fee_raw,
        pool.fee_bps,
    )
    .map_err(DexError::MathError)?;
    if amount_out.is_zero() {
        return Err(DexError::InvalidPool {
            reason: "amount_out computed as zero".to_string(),
        });
    }

    let mut balances_after = pool.balances.clone();
    balances_after[token_in_index] = balances_after[token_in_index]
        .checked_add(amount_in)
        .ok_or_else(|| DexError::MathError(MathError::Overflow {
            operation: "curve.quote_exact_input.balances_after".to_string(),
            inputs: vec![],
            context: format!("balance[{}] + amount_in", token_in_index),
        }))?;
    balances_after[token_out_index] = balances_after[token_out_index]
        .checked_sub(amount_out)
        .ok_or_else(|| DexError::MathError(MathError::Underflow {
            operation: "curve.quote_exact_input.balances_after".to_string(),
            inputs: vec![],
            context: format!("balance[{}] - amount_out", token_out_index),
        }))?;

    let xp_after = math::stableswap_xp_from_rates(&balances_after, &rates).map_err(DexError::MathError)?;
    let spot_after = math::calculate_curve_price(
        token_in_index,
        token_out_index,
        &xp_after,
        &rates,
        pool.variant,
        pool.amplification,
    )
    .map_err(DexError::MathError)?;

    let execution = execution_price_wad(amount_in, amount_out).map_err(DexError::MathError)?;
    let impact_bps = price_impact_bps(spot_before, spot_after).map_err(DexError::MathError)?;

    Ok(CurveExactInQuote {
        amount_in,
        amount_out,
        execution_price_wad: execution,
        price_impact_bps: impact_bps,
        token_in_index,
        token_out_index,
        balances_before: pool.balances.clone(),
        balances_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pool() -> CurvePoolSnapshot {
        CurvePoolSnapshot {
            balances: vec![
                U256::from(1_000_000_000_000u64),
                U256::from(1_000_000_000_000u64),
            ],
            decimals: vec![18, 18],
            stored_rates: None,
            variant: StableswapMathVariant::Vyper02ThreePool,
            amplification: U256::from(1000u64),
            fee_raw: U256::ZERO,
            fee_bps: 4,
        }
    }

    #[test]
    fn quote_exact_input_basic() {
        let pool = sample_pool();
        let q = quote_exact_input(&pool, 0, 1, U256::from(1_000_000u64)).unwrap();
        assert!(q.amount_out > U256::ZERO);
        assert_eq!(q.balances_after.len(), 2);
        assert!(q.execution_price_wad > U256::ZERO);
        assert!(q.price_impact_bps <= 10_000);
    }

    #[test]
    fn quote_exact_input_rejects_same_token() {
        let pool = sample_pool();
        let err = quote_exact_input(&pool, 0, 0, U256::from(100u64)).unwrap_err();
        match err {
            DexError::InvalidPool { reason } => assert!(reason.contains("must differ")),
            _ => panic!("expected InvalidPool"),
        }
    }

    #[test]
    fn quote_exact_input_rejects_zero_amount() {
        let pool = sample_pool();
        let err = quote_exact_input(&pool, 0, 1, U256::ZERO).unwrap_err();
        match err {
            DexError::MathError(MathError::InvalidInput { .. }) => {}
            _ => panic!("expected MathError::InvalidInput"),
        }
    }
}

