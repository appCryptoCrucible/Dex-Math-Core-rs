//! Balancer Weighted Pool Mathematics
//!
//! This module implements Balancer's weighted pool mathematical functions for
//! arbitrage and price impact calculations. Balancer uses weighted constant product
//! formula where each token has a weight that determines its share of liquidity.
//!
//! ## Implementation
//!
//! Swap calculations use the `balancer-maths-rust` crate for maximum accuracy and
//! performance. The crate provides production-grade ln/exp functions with proper
//! range reduction for accurate results, especially for unequal weight configurations.
//!
//! ## Key Formulas
//!
//! - **Invariant**: `V = ∏(B_i)^(W_i)` where B_i is balance and W_i is weight
//! - **Swap**: `amount_out = balance_out * (1 - (balance_in / (balance_in + amount_in_with_fee))^(weight_in / weight_out))`
//! - **Spot Price (WAD)**: `(balance_in * weight_out * 10^18) / (balance_out * weight_in)`
//!
//! ## Fixed-Point Scaling
//!
//! All weights and prices use 18-decimal (10^18) fixed-point format for precision.
//! This matches Balancer V2's on-chain representation.

use crate::core::MathError;
use crate::dex::balancer::conversions::{
    map_pool_error_to_math_error, to_alloy_u256, to_primitive_u256,
};
use balancer_maths_rust::pools::weighted::weighted_math::compute_out_given_exact_in;
use primitive_types::U256 as u256;

/// Fixed-point scaling factor (10^18) - standard ERC20/DeFi precision
const SCALE_18: u128 = 1_000_000_000_000_000_000;

/// Calculate swap output amount for Balancer weighted pools
///
/// Implements the weighted constant product formula:
/// `amount_out = balance_out * (1 - (balance_in / (balance_in + amount_in_with_fee))^(weight_in / weight_out))`
///
/// # Arguments
/// * `amount_in` - Input token amount (raw, unscaled)
/// * `balance_in` - Current balance of input token in pool
/// * `balance_out` - Current balance of output token in pool
/// * `weight_in` - Weight of input token (18-decimal format, e.g., 0.5 = 5e17)
/// * `weight_out` - Weight of output token (18-decimal format)
/// * `swap_fee` - Swap fee (18-decimal format, e.g., 0.003 = 3e15)
///
/// # Returns
/// * `Ok(u256)` - Output amount after fees
/// * `Err(MathError)` - If inputs are invalid or calculation fails
pub fn calculate_swap_output(
    amount_in: u256,
    balance_in: u256,
    balance_out: u256,
    weight_in: u256,
    weight_out: u256,
    swap_fee: u256,
) -> Result<u256, MathError> {
    if amount_in == u256::zero() {
        return Ok(u256::zero());
    }
    if balance_in == u256::zero() || balance_out == u256::zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_swap_output".to_string(),
            reason: "Pool balances cannot be zero".to_string(),
            context: "".to_string(),
        });
    }
    if weight_in == u256::zero() || weight_out == u256::zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_swap_output".to_string(),
            reason: "Token weights cannot be zero".to_string(),
            context: "".to_string(),
        });
    }

    let scale = u256::from(SCALE_18);
    if swap_fee >= scale {
        return Err(MathError::InvalidInput {
            operation: "calculate_swap_output".to_string(),
            reason: "swap_fee must be < 1e18".to_string(),
            context: format!("swap_fee={}", swap_fee),
        });
    }

    // Balancer V2: amountInAfterFee = mulDown(amountIn, complement(swapFee))
    let fee_complement = scale
        .checked_sub(swap_fee)
        .ok_or_else(|| MathError::Underflow {
            operation: "calculate_swap_output".to_string(),
            inputs: vec![],
            context: "1e18 - swap_fee".to_string(),
        })?;
    let amount_in_after_fee = amount_in
        .checked_mul(fee_complement)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_swap_output".to_string(),
            inputs: vec![],
            context: "amount_in * fee_complement".to_string(),
        })?
        / scale;

    let balance_in_alloy = to_alloy_u256(balance_in);
    let weight_in_alloy = to_alloy_u256(weight_in);
    let balance_out_alloy = to_alloy_u256(balance_out);
    let weight_out_alloy = to_alloy_u256(weight_out);
    let amount_in_after_fee_alloy = to_alloy_u256(amount_in_after_fee);

    let crate_result = compute_out_given_exact_in(
        &balance_in_alloy,
        &weight_in_alloy,
        &balance_out_alloy,
        &weight_out_alloy,
        &amount_in_after_fee_alloy,
    )
    .map_err(|e| map_pool_error_to_math_error(e, "calculate_swap_output"))?;

    Ok(to_primitive_u256(crate_result))
}

/// Calculate spot price for Balancer weighted pools (WAD-scaled).
///
/// `spot_price = (balance_in / weight_in) / (balance_out / weight_out)`
///            `= (balance_in * weight_out) / (balance_out * weight_in)`
///
/// Result is scaled by `10^18` (multiply ratio by WAD before dividing).
///
/// # Arguments
/// * `balance_in` - Current balance of input token in pool
/// * `balance_out` - Current balance of output token in pool
/// * `weight_in` - Weight of input token (normalized to sum to 1)
/// * `weight_out` - Weight of output token (normalized to sum to 1)
pub fn calculate_balancer_price(
    balance_in: u256,
    balance_out: u256,
    weight_in: u256,
    weight_out: u256,
) -> Result<u256, MathError> {
    if balance_in == u256::zero() || balance_out == u256::zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_balancer_price".to_string(),
            reason: "Pool balances cannot be zero".to_string(),
            context: format!("balance_in={}, balance_out={}", balance_in, balance_out),
        });
    }
    if weight_in == u256::zero() || weight_out == u256::zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_balancer_price".to_string(),
            reason: "Token weights cannot be zero".to_string(),
            context: format!("weight_in={}, weight_out={}", weight_in, weight_out),
        });
    }

    let scale = u256::from(SCALE_18);
    let numerator = balance_in
        .checked_mul(weight_out)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_balancer_price".to_string(),
            inputs: vec![],
            context: "balance_in * weight_out".to_string(),
        })?;
    let denominator = balance_out
        .checked_mul(weight_in)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_balancer_price".to_string(),
            inputs: vec![],
            context: "balance_out * weight_in".to_string(),
        })?;
    if denominator.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "calculate_balancer_price".to_string(),
            context: "balance_out * weight_in".to_string(),
        });
    }
    let scaled = numerator
        .checked_mul(scale)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_balancer_price".to_string(),
            inputs: vec![],
            context: "numerator * 1e18".to_string(),
        })?;
    Ok(scaled / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_swap_output_basic() {
        let weight_50 = u256::from(5) * u256::from(10).pow(u256::from(17));

        let amount_in = u256::from(1000000);
        let balance_in = u256::from(1000000000000u64);
        let balance_out = u256::from(1000000000000000000000u128);
        let swap_fee = u256::from(3) * u256::from(10).pow(u256::from(15));

        let result = calculate_swap_output(
            amount_in,
            balance_in,
            balance_out,
            weight_50,
            weight_50,
            swap_fee,
        );

        assert!(result.is_ok(), "Swap calculation should succeed");
        let amount_out = result.unwrap();
        assert!(
            amount_out > u256::zero(),
            "Should receive some output tokens"
        );
    }

    #[test]
    fn test_calculate_balancer_price() {
        let balance_in = u256::from(1000000);
        let balance_out = u256::from(1000000);
        let weight_in = u256::from(5) * u256::from(10).pow(u256::from(17));
        let weight_out = u256::from(5) * u256::from(10).pow(u256::from(17));

        let result = calculate_balancer_price(balance_in, balance_out, weight_in, weight_out);
        assert!(result.is_ok(), "Price calculation should succeed");

        let price = result.unwrap();
        assert!(
            price > u256::from(9) * u256::from(10).pow(u256::from(17)),
            "Price should be close to 1"
        );
        assert!(
            price < u256::from(11) * u256::from(10).pow(u256::from(17)),
            "Price should be close to 1"
        );
    }

    #[test]
    fn test_zero_input() {
        let result = calculate_swap_output(
            u256::zero(),
            u256::from(1000),
            u256::from(1000),
            u256::from(5) * u256::from(10).pow(u256::from(17)),
            u256::from(5) * u256::from(10).pow(u256::from(17)),
            u256::zero(),
        );
        assert_eq!(
            result.unwrap(),
            u256::zero(),
            "Zero input should return zero output"
        );
    }

    #[test]
    fn test_zero_balance() {
        let result = calculate_swap_output(
            u256::from(100),
            u256::zero(),
            u256::from(1000),
            u256::from(5) * u256::from(10).pow(u256::from(17)),
            u256::from(5) * u256::from(10).pow(u256::from(17)),
            u256::zero(),
        );
        assert!(result.is_err(), "Zero balance should return error");
    }
}
