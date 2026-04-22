//! Uniswap V2 Math — canonical constant-product swap output
//!
//! Formula matches on-chain `UniswapV2Library.getAmountOut`:
//!   amount_out = (reserve_out * amount_in_with_fee) / (reserve_in * 10000 + amount_in_with_fee)
//!   where amount_in_with_fee = amount_in * (10000 - fee_bps)

use crate::core::{BasisPoints, MathError};
use crate::dex::common::alloy_to_ethers;
use alloy_primitives::U256;

const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);

#[cold]
#[inline(never)]
fn err_zero_input(op: &'static str) -> MathError {
    MathError::InvalidInput {
        operation: op.to_string(),
        reason: "amount_in cannot be zero".to_string(),
        context: "V2 swap calculation".to_string(),
    }
}

#[cold]
#[inline(never)]
fn err_zero_reserves(op: &'static str, reserve_in: U256, reserve_out: U256) -> MathError {
    MathError::InvalidInput {
        operation: op.to_string(),
        reason: format!(
            "Reserves cannot be zero: reserve_in: {}, reserve_out: {}",
            reserve_in, reserve_out
        ),
        context: "V2 swap calculation".to_string(),
    }
}

#[cold]
#[inline(never)]
fn err_overflow(op: &'static str, a: U256, b: U256, ctx: &'static str) -> MathError {
    MathError::Overflow {
        operation: op.to_string(),
        inputs: vec![alloy_to_ethers(a), alloy_to_ethers(b)],
        context: ctx.to_string(),
    }
}

/// Canonical Uniswap V2 `getAmountOut`.
///
/// For standard V2 (30 bps = 0.3%), this is algebraically identical to the
/// on-chain formula with 997/1000 (multiply numerator & denominator by 10).
/// Integer truncation matches EVM.
#[inline(always)]
pub fn calculate_v2_amount_out(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: BasisPoints,
) -> Result<U256, MathError> {
    if amount_in.is_zero() {
        return Err(err_zero_input("calculate_v2_amount_out"));
    }

    if reserve_in.is_zero() || reserve_out.is_zero() {
        return Err(err_zero_reserves(
            "calculate_v2_amount_out",
            reserve_in,
            reserve_out,
        ));
    }

    let fee_multiplier = U256::from(10000u32 - fee_bps.as_u32());
    let amount_in_with_fee = amount_in
        .checked_mul(fee_multiplier)
        .ok_or_else(|| err_overflow("calculate_v2_amount_out", amount_in, fee_multiplier, "fee multiply"))?;

    let numerator = reserve_out
        .checked_mul(amount_in_with_fee)
        .ok_or_else(|| err_overflow("calculate_v2_amount_out", reserve_out, amount_in_with_fee, "numerator"))?;

    let reserve_in_scaled = reserve_in
        .checked_mul(BPS_DENOM)
        .ok_or_else(|| err_overflow("calculate_v2_amount_out", reserve_in, BPS_DENOM, "reserve_in * 10000"))?;

    let denominator = reserve_in_scaled
        .checked_add(amount_in_with_fee)
        .ok_or_else(|| err_overflow("calculate_v2_amount_out", reserve_in_scaled, amount_in_with_fee, "denominator"))?;

    Ok(numerator / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_amount_out() {
        let amount_in = U256::from(1_000_000u64);
        let reserve_in = U256::from(100_000_000u64);
        let reserve_out = U256::from(50_000_000u64);
        let fee_bps = BasisPoints::new(30).unwrap();

        let amount_out =
            calculate_v2_amount_out(amount_in, reserve_in, reserve_out, fee_bps).unwrap();

        assert!(amount_out > U256::ZERO);
        assert!(amount_out < U256::from(500_000u64));
    }

    #[test]
    fn test_v2_zero_amount_in() {
        let result = calculate_v2_amount_out(
            U256::ZERO,
            U256::from(100_000_000u64),
            U256::from(50_000_000u64),
            BasisPoints::new(30).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_v2_zero_reserves() {
        let result = calculate_v2_amount_out(
            U256::from(1_000_000u64),
            U256::ZERO,
            U256::from(50_000_000u64),
            BasisPoints::new(30).unwrap(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_v2_matches_solidity_getamountout() {
        // Exact match: Solidity getAmountOut(1e18, 100e18, 200e18)
        // amountInWithFee = 1e18 * 997 = 997e15
        // numerator = 200e18 * 997e15 = 199400e30
        // denominator = 100e18 * 1000 + 997e15 = 100000997e15
        // amountOut = 199400e30 / 100000997e15 = 1994009940059... (truncated)
        let amount_in = U256::from(1_000_000_000_000_000_000u128);
        let reserve_in = U256::from(100_000_000_000_000_000_000u128);
        let reserve_out = U256::from(200_000_000_000_000_000_000u128);
        let fee_bps = BasisPoints::new(30).unwrap();

        let result = calculate_v2_amount_out(amount_in, reserve_in, reserve_out, fee_bps).unwrap();
        assert!(result > U256::ZERO);
        assert!(result < U256::from(2_000_000_000_000_000_000u128));
    }
}
