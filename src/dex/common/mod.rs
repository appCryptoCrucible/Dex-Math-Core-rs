//! Common DEX calculations shared across all DEX types

use crate::core::MathError;
use ethers_core::types::U256;

use ethers_core::types::U256 as EthersU256;
use alloy_primitives::U256 as AlloyU256;

/// Zero-cost ethers -> alloy U256 conversion via limb copy.
/// Both types are [u64; 4] little-endian internally.
#[inline(always)]
pub fn ethers_to_alloy(v: EthersU256) -> AlloyU256 {
    AlloyU256::from_limbs(v.0)
}

/// Zero-cost alloy -> ethers U256 conversion via limb copy.
#[inline(always)]
pub fn alloy_to_ethers(v: AlloyU256) -> EthersU256 {
    EthersU256(v.into_limbs())
}

/// Calculate exact exchange rate from pool reserves
/// Returns rate in basis points (token_b per token_a * 10000)
pub fn calculate_exact_rate(
    reserve_a: U256,
    reserve_b: U256,
    decimals_a: u8,
    decimals_b: u8,
) -> Result<U256, MathError> {
    if reserve_a.is_zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_exact_rate".to_string(),
            reason: "Reserve A cannot be zero".to_string(),
            context: "Rate calculation".to_string(),
        });
    }

    // Normalize for decimals: rate = (reserve_b * 10^decimals_a * 10000) / (reserve_a * 10^decimals_b)
    let numerator = reserve_b
        .checked_mul(U256::from(10u128.pow(decimals_a as u32)))
        .and_then(|v| v.checked_mul(U256::from(10000)))
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_exact_rate".to_string(),
            inputs: vec![reserve_b],
            context: "Numerator calculation".to_string(),
        })?;

    let denominator = reserve_a
        .checked_mul(U256::from(10u128.pow(decimals_b as u32)))
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_exact_rate".to_string(),
            inputs: vec![reserve_a],
            context: "Denominator calculation".to_string(),
        })?;

    numerator
        .checked_div(denominator)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_exact_rate".to_string(),
            context: "Final division".to_string(),
        })
}
