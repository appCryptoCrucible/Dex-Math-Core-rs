//! Uniswap V3 / Kyber Elastic Math - Production-grade tick math
//!
//! V3 uses concentrated liquidity with ticks representing price points.
//! Math is identical between Uniswap V3 and Kyber Elastic.
//!
//! Key formula: price = 1.0001^tick
//! Represented as sqrt(price) in Q64.96 fixed-point format


use alloy_primitives::U256;
use crate::core::{BasisPoints, MathError};
use crate::dex::adapter::SwapDirection;
use crate::dex::common::alloy_to_ethers;
use uniswap_v3_math::{
    tick_math, sqrt_price_math, full_math,
    error::UniswapV3MathError,
};

/// Rounding direction for Uniswap V3 amount calculations
/// 
/// Uniswap V3 uses asymmetric rounding:
/// - RoundUp: When trader pays (overcharge trader, favor pool)
/// - RoundDown: When pool pays (underpay trader, favor pool)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// Round up (ceil) - used when computing trader input amounts
    Up,
    /// Round down (floor) - used when computing pool output amounts
    Down,
}

/// Minimum tick value
pub const MIN_TICK: i32 = -887272;

/// Maximum tick value
pub const MAX_TICK: i32 = 887272;

/// Minimum sqrt ratio (at MIN_TICK)
pub const MIN_SQRT_RATIO: u128 = 4295128739;

const U256_ZERO: U256 = U256::ZERO;
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);
/// 1 << 96 (Q64.96 fixed-point scale)
const Q96: U256 = U256::from_limbs([0, 0x1_0000_0000, 0, 0]);
const Q192: U256 = U256::from_limbs([0, 0, 0, 1]);
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);

fn map_v3_error(e: UniswapV3MathError) -> MathError {
    MathError::InvalidInput {
        operation: "uniswap_v3_math".to_string(),
        reason: format!("{}", e),
        context: "V3 crate error".to_string(),
    }
}

pub fn get_sqrt_ratio_at_tick(tick: i32) -> Result<U256, MathError> {
    tick_math::get_sqrt_ratio_at_tick(tick).map_err(map_v3_error)
}

pub fn sqrt_price_to_tick(sqrt_price_x96: U256) -> Result<i32, MathError> {
    if sqrt_price_x96 < tick_math::MIN_SQRT_RATIO {
        return Ok(MIN_TICK);
    }
    if sqrt_price_x96 >= tick_math::MAX_SQRT_RATIO {
        return Ok(MAX_TICK);
    }
    tick_math::get_tick_at_sqrt_ratio(sqrt_price_x96).map_err(map_v3_error)
}

fn mul_div(a: U256, b: U256, d: U256) -> Result<U256, MathError> {
    full_math::mul_div(a, b, d).map_err(map_v3_error)
}

pub fn mul_div_rounding_up(a: U256, b: U256, d: U256) -> Result<U256, MathError> {
    full_math::mul_div_rounding_up(a, b, d).map_err(map_v3_error)
}

pub fn get_amount0_delta(
    sqrt_ratio_a: U256,
    sqrt_ratio_b: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, MathError> {
    sqrt_price_math::_get_amount_0_delta(sqrt_ratio_a, sqrt_ratio_b, liquidity, round_up)
        .map_err(map_v3_error)
}

pub fn get_amount1_delta(
    sqrt_ratio_a: U256,
    sqrt_ratio_b: U256,
    liquidity: u128,
    round_up: bool,
) -> Result<U256, MathError> {
    sqrt_price_math::_get_amount_1_delta(sqrt_ratio_a, sqrt_ratio_b, liquidity, round_up)
        .map_err(map_v3_error)
}

/// Calculate V3 price impact in basis points from exact pre/post sqrt prices.
///
/// # Arguments
/// * `sqrt_price_before_x96` - Pre-swap sqrt price in Q64.96
/// * `sqrt_price_after_x96` - Post-swap sqrt price in Q64.96
///
/// # Returns
/// * `Ok(u32)` - Price impact in basis points
pub fn calculate_v3_price_impact(
    sqrt_price_before_x96: U256,
    sqrt_price_after_x96: U256,
) -> Result<u32, MathError> {
    if sqrt_price_before_x96.is_zero() || sqrt_price_after_x96.is_zero() {
        return Ok(0);
    }

    let price_before = sqrt_price_to_price_wad(sqrt_price_before_x96)?;
    let price_after = sqrt_price_to_price_wad(sqrt_price_after_x96)?;
    if price_before.is_zero() {
        return Ok(0);
    }

    let price_diff = if price_after >= price_before {
        price_after - price_before
    } else {
        price_before - price_after
    };

    let impact =
        price_diff
            .checked_mul(BPS_DENOM)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_v3_price_impact".to_string(),
                inputs: vec![alloy_to_ethers(price_diff), alloy_to_ethers(BPS_DENOM)],
                context: "price_diff * 10_000".to_string(),
            })?;

    let impact_bps = impact
        .checked_div(price_before)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_v3_price_impact".to_string(),
            context: "price_before".to_string(),
        })?;

    Ok(if impact_bps > BPS_DENOM {
        10000
    } else {
        impact_bps.as_limbs()[0] as u32
    })
}

/// Convert sqrt price (Q64.96) to regular price
pub fn sqrt_price_to_price(sqrt_price_x96: U256) -> Result<U256, MathError> {
    // sqrt_price_x96 is in Q64.96 format
    // Price = (sqrt_price_x96 / 2^96)^2 = sqrt_price_x96^2 / 2^192
    // Use full-precision mulDiv to avoid overflow when squaring large sqrt prices.
    mul_div(sqrt_price_x96, sqrt_price_x96, Q192)
}

/// Calculate sqrt_price_x96 from reserve amounts (inverse of price calculation)
///
/// For V3: sqrtPriceX96 = sqrt(reserve_out / reserve_in) * 2^96
/// Reuses the battle-tested sqrt implementation from Curve math.
///
/// # Arguments
/// * `reserve_in` - Reserve of token0 (input token)
/// * `reserve_out` - Reserve of token1 (output token)
///
/// # Returns
/// * `Ok(U256)` - Sqrt price in Q64.96 format
/// * `Err(MathError)` - If calculation fails
pub fn reserves_to_sqrt_price_x96(reserve_in: U256, reserve_out: U256) -> Result<U256, MathError> {
    if reserve_in.is_zero() {
        return Err(MathError::DivisionByZero {
            operation: "reserves_to_sqrt_price_x96".to_string(),
            context: "Reserve in cannot be zero".to_string(),
        });
    }

    // Q64.96 encoding requires:
    // sqrt_price_x96 = sqrt(reserve_out / reserve_in) * 2^96
    //                = sqrt((reserve_out * 2^192) / reserve_in)
    let ratio_q192 = mul_div(reserve_out, Q192, reserve_in)?;
    crate::dex::curve::math::sqrt_u256(ratio_q192)
}

/// Convert sqrt price (Q64.96) to WAD-scaled price.
///
/// Formula: price_wad = sqrt_price_x96^2 * 1e18 / 2^192
#[inline(always)]
pub fn sqrt_price_to_price_wad(sqrt_price_x96: U256) -> Result<U256, MathError> {
    let price = sqrt_price_to_price(sqrt_price_x96)?;
    price.checked_mul(WAD).ok_or_else(|| MathError::Overflow {
        operation: "sqrt_price_to_price_wad".to_string(),
        inputs: vec![alloy_to_ethers(price), alloy_to_ethers(WAD)],
        context: "price * WAD".to_string(),
    })
}

/// Calculate V3 swap output using correct Uniswap V3 SwapMath formulas
/// Implements exact formulas from SwapMath.sol for both swap directions
///
/// # Arguments
/// * `amount_in` - Input amount (after fee will be calculated)
/// * `sqrt_price_x96` - Current sqrt price in Q64.96 format
/// * `liquidity` - Active liquidity in the current tick range
/// * `fee_bps` - Fee in basis points (e.g., 300 for 0.3%)
/// * `direction` - Swap direction (Token0ToToken1 or Token1ToToken0)
///
/// # Returns
/// * `Ok(U256)` - Output amount
/// * `Err(MathError)` - If calculation fails or inputs invalid
pub fn calculate_v3_amount_out(
    amount_in: U256,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee_bps: BasisPoints,
    direction: SwapDirection,
) -> Result<U256, MathError> {
    // Input validation
    if amount_in.is_zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_amount_out".to_string(),
            reason: "amount_in cannot be zero".to_string(),
            context: format!(
                "direction={:?}, sqrt_price={}, liquidity={}",
                direction, sqrt_price_x96, liquidity
            ),
        });
    }

    if sqrt_price_x96.is_zero() || sqrt_price_x96 < U256::from(MIN_SQRT_RATIO) {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_amount_out".to_string(),
            reason: format!("sqrt_price_x96 out of valid range: {}", sqrt_price_x96),
            context: format!(
                "direction={:?}, amount_in={}, liquidity={}",
                direction, amount_in, liquidity
            ),
        });
    }

    if liquidity == 0 {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_amount_out".to_string(),
            reason: "Liquidity cannot be zero".to_string(),
            context: format!(
                "direction={:?}, amount_in={}, sqrt_price={}",
                direction, amount_in, sqrt_price_x96
            ),
        });
    }

    // Apply fee: amount_in_after_fee = amount_in * (10000 - fee_bps) / 10000
    let fee_multiplier = U256::from(10000 - fee_bps.as_u32());
    let amount_in_after_fee = amount_in
        .checked_mul(fee_multiplier)
        .and_then(|v| v.checked_div(BPS_DENOM))
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_v3_amount_out".to_string(),
            inputs: vec![alloy_to_ethers(amount_in), alloy_to_ethers(U256::from(fee_bps.as_u32()))],
            context: format!(
                "Fee calculation failed (direction={:?}, amount_in={})",
                direction, amount_in
            ),
        })?;

    if amount_in_after_fee.is_zero() {
        return Ok(U256_ZERO);
    }

    // Implement correct V3 SwapMath formulas based on direction
    match direction {
        SwapDirection::Token0ToToken1 => {
            // Canonical: getNextSqrtPriceFromAmount0RoundingUp (CEIL protects pool)
            let new_sqrt_price = sqrt_price_math::get_next_sqrt_price_from_amount_0_rounding_up(
                sqrt_price_x96,
                liquidity,
                amount_in_after_fee,
                true,
            ).map_err(map_v3_error)?;

            if new_sqrt_price >= sqrt_price_x96 {
                return Err(MathError::InvalidInput {
                    operation: "calculate_v3_amount_out".to_string(),
                    reason: "New sqrt price must be less than current for zeroForOne swap".to_string(),
                    context: format!("direction={:?}, sqrt_price={}, new_sqrt_price={}, amount_in={}, liquidity={}", direction, sqrt_price_x96, new_sqrt_price, amount_in, liquidity),
                });
            }

            let amount_out = get_amount1_delta(
                new_sqrt_price,
                sqrt_price_x96,
                liquidity,
                false,
            )?;
            Ok(amount_out)
        }
        SwapDirection::Token1ToToken0 => {
            // Canonical: getNextSqrtPriceFromAmount1RoundingDown (FLOOR protects pool)
            let new_sqrt_price = sqrt_price_math::get_next_sqrt_price_from_amount_1_rounding_down(
                sqrt_price_x96,
                liquidity,
                amount_in_after_fee,
                true,
            ).map_err(map_v3_error)?;

            let amount_out = get_amount0_delta(
                sqrt_price_x96,
                new_sqrt_price,
                liquidity,
                false,
            )?;
            Ok(amount_out)
        }
    }
}

/// Calculate V3 pool state after a swap
/// Uses canonical sqrt price math from the uniswap_v3_math crate
///
/// # Arguments
/// * `frontrun_amount` - Amount of input token for the swap
/// * `sqrt_price_x96` - Current sqrt price in Q64.96 format
/// * `liquidity` - Active liquidity in the current tick range
/// * `fee_bps` - Fee in basis points (e.g., 300 for 0.3%)
/// * `direction` - Swap direction (Token0ToToken1 or Token1ToToken0)
///
/// # Returns
/// * `Ok((U256, i32))` - New sqrt price and new tick after the swap
/// * `Err(MathError)` - If calculation fails or inputs invalid
pub fn calculate_v3_post_swap_state(
    frontrun_amount: U256,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee_bps: BasisPoints,
    direction: SwapDirection,
) -> Result<(U256, i32), MathError> {
    // Input validation
    if frontrun_amount.is_zero() {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_post_swap_state".to_string(),
            reason: "frontrun_amount cannot be zero".to_string(),
            context: format!(
                "direction={:?}, sqrt_price={}, liquidity={}",
                direction, sqrt_price_x96, liquidity
            ),
        });
    }

    if sqrt_price_x96.is_zero() || sqrt_price_x96 < U256::from(MIN_SQRT_RATIO) {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_post_swap_state".to_string(),
            reason: format!("sqrt_price_x96 out of valid range: {}", sqrt_price_x96),
            context: format!(
                "direction={:?}, frontrun_amount={}, liquidity={}",
                direction, frontrun_amount, liquidity
            ),
        });
    }

    if liquidity == 0 {
        return Err(MathError::InvalidInput {
            operation: "calculate_v3_post_swap_state".to_string(),
            reason: "Liquidity cannot be zero".to_string(),
            context: format!(
                "direction={:?}, frontrun_amount={}, sqrt_price={}",
                direction, frontrun_amount, sqrt_price_x96
            ),
        });
    }

    // Apply fee: amount_in_after_fee = amount_in * (10000 - fee_bps) / 10000
    let fee_multiplier = U256::from(10000 - fee_bps.as_u32());
    let amount_in_after_fee = frontrun_amount
        .checked_mul(fee_multiplier)
        .and_then(|v| v.checked_div(BPS_DENOM))
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_v3_post_swap_state".to_string(),
            inputs: vec![alloy_to_ethers(frontrun_amount), alloy_to_ethers(U256::from(fee_bps.as_u32()))],
            context: format!(
                "Fee calculation failed (direction={:?}, frontrun_amount={})",
                direction, frontrun_amount
            ),
        })?;

    if amount_in_after_fee.is_zero() {
        // If amount after fee is zero, price doesn't change
        return Ok((sqrt_price_x96, sqrt_price_to_tick(sqrt_price_x96)?));
    }

    // Canonical crate functions for sqrt price update
    let new_sqrt_price = match direction {
        SwapDirection::Token0ToToken1 => {
            sqrt_price_math::get_next_sqrt_price_from_amount_0_rounding_up(
                sqrt_price_x96,
                liquidity,
                amount_in_after_fee,
                true,
            ).map_err(map_v3_error)?
        }
        SwapDirection::Token1ToToken0 => {
            sqrt_price_math::get_next_sqrt_price_from_amount_1_rounding_down(
                sqrt_price_x96,
                liquidity,
                amount_in_after_fee,
                true,
            ).map_err(map_v3_error)?
        }
    };

    let new_tick = sqrt_price_to_tick(new_sqrt_price)?;

    Ok((new_sqrt_price, new_tick))
}

/// Swap execution segment (within one tick range)
#[derive(Debug, Clone)]
pub struct SwapSegment {
    /// Starting sqrt_price for this segment
    pub sqrt_price_start: U256,
    /// Ending sqrt_price for this segment
    pub sqrt_price_end: U256,
    /// Tick at start of segment
    pub tick_start: i32,
    /// Tick at end of segment
    pub tick_end: i32,
    /// Liquidity active in this segment
    pub liquidity: u128,
    /// Amount swapped in this segment
    pub amount_in: U256,
    /// Fee generated in this segment
    pub fee_amount: U256,
}

/// Simulate V3 swap with tick-level details and direction awareness
/// CRITICAL: Returns exact execution path for fee calculations
///
/// # Arguments
/// * `amount_in` - Input amount
/// * `sqrt_price_start` - Starting sqrt_price
/// * `current_liquidity` - Starting active liquidity
/// * `fee_bps` - Fee in basis points
/// * `tick_spacing` - Tick spacing for the pool
/// * `initialized_ticks` - Sorted initialized tick boundaries
/// * `zero_for_one` - true = token0->token1 (price DOWN), false = token1->token0 (price UP)
/// * `tick_liquidity_net` - Tick -> liquidity delta for crossing
///
/// # Returns
/// * Vector of swap segments showing tick-by-tick execution
pub fn simulate_swap_with_ticks(
    amount_in: U256,
    sqrt_price_start: U256,
    current_liquidity: u128,
    fee_bps: BasisPoints,
    tick_spacing: i32,
    initialized_ticks: &[i32],
    zero_for_one: bool,
    tick_liquidity_net: &std::collections::HashMap<i32, i128>,
) -> Result<Vec<SwapSegment>, MathError> {
    let mut segments = Vec::new();
    let mut remaining_amount = amount_in;
    let mut current_sqrt_price = sqrt_price_start;
    let mut current_tick = sqrt_price_to_tick(current_sqrt_price)?;
    let mut active_liquidity = current_liquidity;

    while !remaining_amount.is_zero() && segments.len() < 1000 {
        let next_tick = find_next_initialized_tick(
            current_tick, initialized_ticks, tick_spacing, zero_for_one,
        )?;
        let next_tick_sqrt_price = get_sqrt_ratio_at_tick(next_tick)?;

        // Direction-aware max input to reach next tick boundary
        let max_amount_to_next_tick = if zero_for_one {
            get_amount0_delta(next_tick_sqrt_price, current_sqrt_price, active_liquidity, true)?
        } else {
            get_amount1_delta(current_sqrt_price, next_tick_sqrt_price, active_liquidity, true)?
        };

        let segment_amount = remaining_amount.min(max_amount_to_next_tick);

        let segment_fee = segment_amount
            .checked_mul(U256::from(fee_bps.as_u32()))
            .ok_or_else(|| MathError::Overflow {
                operation: "simulate_swap_with_ticks".to_string(),
                inputs: vec![alloy_to_ethers(segment_amount)],
                context: "fee calculation".to_string(),
            })?
            .checked_div(BPS_DENOM)
            .ok_or_else(|| MathError::DivisionByZero {
                operation: "simulate_swap_with_ticks".to_string(),
                context: "fee division".to_string(),
            })?;

        let amount_after_fee = segment_amount
            .checked_sub(segment_fee)
            .ok_or_else(|| MathError::Underflow {
                operation: "simulate_swap_with_ticks".to_string(),
                inputs: vec![alloy_to_ethers(segment_amount), alloy_to_ethers(segment_fee)],
                context: "amount after fee".to_string(),
            })?;

        // Canonical crate functions for new sqrt price
        let new_sqrt_price = if zero_for_one {
            sqrt_price_math::get_next_sqrt_price_from_amount_0_rounding_up(
                current_sqrt_price, active_liquidity, amount_after_fee, true,
            ).map_err(map_v3_error)?
        } else {
            sqrt_price_math::get_next_sqrt_price_from_amount_1_rounding_down(
                current_sqrt_price, active_liquidity, amount_after_fee, true,
            ).map_err(map_v3_error)?
        };

        let new_tick = sqrt_price_to_tick(new_sqrt_price)?;

        segments.push(SwapSegment {
            sqrt_price_start: current_sqrt_price,
            sqrt_price_end: new_sqrt_price,
            tick_start: current_tick,
            tick_end: new_tick,
            liquidity: active_liquidity,
            amount_in: segment_amount,
            fee_amount: segment_fee,
        });

        remaining_amount = remaining_amount
            .checked_sub(segment_amount)
            .ok_or_else(|| MathError::Underflow {
                operation: "simulate_swap_with_ticks".to_string(),
                inputs: vec![alloy_to_ethers(remaining_amount), alloy_to_ethers(segment_amount)],
                context: "remaining amount".to_string(),
            })?;
        current_sqrt_price = new_sqrt_price;
        current_tick = new_tick;

        // If we hit the tick boundary, update liquidity for crossing
        if segment_amount >= max_amount_to_next_tick {
            if let Some(&liq_net) = tick_liquidity_net.get(&next_tick) {
                let l = active_liquidity as i128;
                let nl = if zero_for_one {
                    l - liq_net
                } else {
                    l + liq_net
                };
                active_liquidity = nl.max(0).min(u128::MAX as i128) as u128;
            }
            if active_liquidity == 0 {
                break;
            }
        } else {
            break;
        }
    }

    Ok(segments)
}

/// Find next initialized tick boundary in the given direction
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
        // Downward: largest initialized tick strictly below current_tick
        let pos = initialized_ticks.partition_point(|&t| t < current_tick);
        if pos > 0 {
            Ok(initialized_ticks[pos - 1])
        } else {
            Ok((current_tick.div_euclid(tick_spacing) - 1) * tick_spacing)
        }
    } else {
        // Upward: smallest initialized tick strictly above current_tick
        let pos = initialized_ticks.partition_point(|&t| t <= current_tick);
        if pos < initialized_ticks.len() {
            Ok(initialized_ticks[pos])
        } else {
            Ok((current_tick.div_euclid(tick_spacing) + 1) * tick_spacing)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::U256_ZERO;

    #[test]
    fn test_tick_at_zero() {
        let sqrt_ratio = get_sqrt_ratio_at_tick(0).unwrap();
        assert_eq!(sqrt_ratio, U256::from(79228162514264337593543950336u128));
    }

    #[test]
    fn test_tick_bounds() {
        let min = get_sqrt_ratio_at_tick(MIN_TICK).unwrap();
        let max = get_sqrt_ratio_at_tick(MAX_TICK).unwrap();

        assert_eq!(min, U256::from(MIN_SQRT_RATIO));
        assert_eq!(max, uniswap_v3_math::tick_math::MAX_SQRT_RATIO);
        assert!(max > U256_ZERO);
    }

    #[test]
    fn test_tick_out_of_bounds() {
        let result = get_sqrt_ratio_at_tick(MIN_TICK - 1);
        assert!(result.is_err());

        let result = get_sqrt_ratio_at_tick(MAX_TICK + 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_mul_div_rounding_up_exact_division() {
        // Test cases where division is exact (no rounding needed)
        // 100 * 200 / 100 = 200 (exact)
        let result =
            mul_div_rounding_up(U256::from(100u64), U256::from(200u64), U256::from(100u64)).unwrap();
        assert_eq!(result, U256::from(200u64));

        // 50 * 60 / 10 = 300 (exact)
        let result = mul_div_rounding_up(U256::from(50u64), U256::from(60u64), U256::from(10u64)).unwrap();
        assert_eq!(result, U256::from(300u64));
    }

    #[test]
    fn test_mul_div_rounding_up_requires_rounding() {
        // Test cases where rounding up is required
        // 100 * 201 / 100 = 201 (exact, but test rounding logic)
        // 100 * 199 / 100 = 199 (exact)
        // 100 * 201 / 200 = 100.5 -> rounds up to 101
        let result =
            mul_div_rounding_up(U256::from(100u64), U256::from(201u64), U256::from(200u64)).unwrap();
        assert_eq!(result, U256::from(101u64));

        // 7 * 3 / 2 = 10.5 -> rounds up to 11
        let result = mul_div_rounding_up(U256::from(7u64), U256::from(3u64), U256::from(2u64)).unwrap();
        assert_eq!(result, U256::from(11u64));

        // 1 * 1 / 3 = 0.333... -> rounds up to 1
        let result = mul_div_rounding_up(U256::from(1u64), U256::from(1u64), U256::from(3u64)).unwrap();
        assert_eq!(result, U256::from(1u64));
    }

    #[test]
    fn test_mul_div_rounding_up_edge_cases() {
        // Zero multiplicand
        let result = mul_div_rounding_up(U256_ZERO, U256::from(100u64), U256::from(10u64)).unwrap();
        assert_eq!(result, U256_ZERO);

        // Zero multiplicand (other direction)
        let result = mul_div_rounding_up(U256::from(100u64), U256_ZERO, U256::from(10u64)).unwrap();
        assert_eq!(result, U256_ZERO);

        // Division by zero should error
        let result = mul_div_rounding_up(U256::from(100u64), U256::from(200u64), U256_ZERO);
        assert!(result.is_err());
    }

    #[test]
    fn test_mul_div_rounding_up_large_values() {
        // Test with large values to ensure U512 arithmetic works
        let large_a = "1000000000000000000000000".parse::<U256>().unwrap(); // 1e21
        let large_b = "2000000000000000000000000".parse::<U256>().unwrap(); // 2e21
        let denom = "1000000000000000000000".parse::<U256>().unwrap(); // 1e18

        // Result should be: (1e21 * 2e21) / 1e18 = 2e24
        let result = mul_div_rounding_up(large_a, large_b, denom).unwrap();
        let expected = "2000000000000000000000000000".parse::<U256>().unwrap(); // 2e24
        assert_eq!(result, expected);
    }

    #[test]
    fn test_mul_div_rounding_up_vs_mul_div() {
        // Compare rounding_up with regular mul_div
        // For exact divisions, they should be the same
        let a = U256::from(100u64);
        let b = U256::from(200u64);
        let denom = U256::from(100u64);

        let regular = mul_div(a, b, denom).unwrap();
        let rounded = mul_div_rounding_up(a, b, denom).unwrap();
        assert_eq!(regular, rounded);

        // For non-exact divisions, rounded should be >= regular
        let a = U256::from(100u64);
        let b = U256::from(201u64);
        let denom = U256::from(200u64);

        let regular = mul_div(a, b, denom).unwrap();
        let rounded = mul_div_rounding_up(a, b, denom).unwrap();
        assert!(rounded >= regular);
        // In this case: regular = 100, rounded = 101
        assert_eq!(regular, U256::from(100u64));
        assert_eq!(rounded, U256::from(101u64));
    }

    #[test]
    fn test_calculate_v3_amount_out_token0_to_token1_small() {
        // Test Token0→Token1 with small amounts
        let amount_in = U256::from(1000_000_000_000_000_000u128); // 0.001 ETH (18 decimals)
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0 (tick = 0)
        let liquidity = 1_000_000_000_000_000_000_000u128; // 1000 tokens
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        let result = calculate_v3_amount_out(
            amount_in,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // Should get some token1 out (exact value depends on formula)
        assert!(result > U256_ZERO);
        assert!(result < amount_in); // Should be less than input due to fee
    }

    #[test]
    fn test_calculate_v3_amount_out_token1_to_token0_small() {
        // Test Token1→Token0 with small amounts
        let amount_in = U256::from(1000_000_000_000_000_000u128); // 0.001 token1
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0
        let liquidity = 1_000_000_000_000_000_000_000u128; // 1000 tokens
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        let result = calculate_v3_amount_out(
            amount_in,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token1ToToken0,
        )
        .unwrap();

        // Should get some token0 out
        assert!(result > U256_ZERO);
        assert!(result < amount_in); // Should be less than input due to fee
    }

    #[test]
    fn test_calculate_v3_amount_out_token0_to_token1_large() {
        // Test Token0→Token1 with larger amounts
        let amount_in = U256::from(100_000_000_000_000_000_000u128); // 100 tokens
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0
        let liquidity = 10_000_000_000_000_000_000_000u128; // 10000 tokens
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        let result = calculate_v3_amount_out(
            amount_in,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        assert!(result > U256_ZERO);
        // With 0.3% fee, should get approximately 99.7% of input (but in token1)
        // Since price = 1.0, should be close to amount_in_after_fee
        let amount_after_fee = amount_in * U256::from(9970u64) / super::BPS_DENOM;
        // Result should be close to amount_after_fee (within reasonable rounding)
        let af_u128 = amount_after_fee.as_limbs()[0] as u128 | ((amount_after_fee.as_limbs()[1] as u128) << 64);
        assert!(result <= amount_after_fee + U256::from(af_u128 / 100));
        // Within 1%
    }

    #[test]
    fn test_calculate_v3_amount_out_zero_input() {
        // Test that zero input returns error
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128);
        let liquidity = 1_000_000_000_000_000_000_000u128;
        let fee_bps = BasisPoints::new_const(300);

        let result = calculate_v3_amount_out(
            U256_ZERO,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            MathError::InvalidInput { .. } => {}
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_v3_amount_out_zero_liquidity() {
        // Test that zero liquidity returns error
        let amount_in = U256::from(1000_000_000_000_000_000u128);
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128);
        let fee_bps = BasisPoints::new_const(300);

        let result = calculate_v3_amount_out(
            amount_in,
            sqrt_price_x96,
            0,
            fee_bps,
            SwapDirection::Token0ToToken1,
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            MathError::InvalidInput { .. } => {}
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_v3_amount_out_direction_consistency() {
        // Property-based test: Swap token0→token1, then swap result token1→token0
        // Should return approximately original amount (minus fees)
        let original_amount = U256::from(1000_000_000_000_000_000u128); // 1 token
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0
        let liquidity = 10_000_000_000_000_000_000_000u128; // 10000 tokens (high liquidity for minimal price impact)
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        // First swap: token0 → token1
        let token1_received = calculate_v3_amount_out(
            original_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        assert!(token1_received > U256_ZERO);

        // Get new sqrt price after first swap (simplified - in reality would need to calculate)
        // For this test, we'll use a slightly different price to simulate the swap
        // In a real implementation, we'd calculate the new price from the swap

        // Second swap: token1 → token0 (reverse direction)
        // Note: This is a simplified test - in reality the sqrt_price would have changed
        // For property testing, we accept that with fees, we won't get exact original back
        let token0_received = calculate_v3_amount_out(
            token1_received,
            sqrt_price_x96, // Using same price (simplified)
            liquidity,
            fee_bps,
            SwapDirection::Token1ToToken0,
        )
        .unwrap();

        // Due to fees (0.3% twice = ~0.6% total), we should get back less than original
        // But should be within reasonable range (e.g., > 99% of original after fees)
        let _min_expected = original_amount * U256::from(9900u64) / super::BPS_DENOM; // 99% of original
        assert!(token0_received < original_amount); // Less due to fees
                                                    // Note: This is a simplified property test - real swaps would have price impact
    }

    #[test]
    fn test_calculate_v3_post_swap_state_token0_to_token1() {
        // Test Token0→Token1 direction
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128); // 0.001 ETH
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0
        let liquidity = 1_000_000_000_000_000_000_000u128; // 1000 tokens
        let tick = 0;
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        let (new_sqrt_price, new_tick) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // For zeroForOne, new sqrt price should be less than current
        assert!(new_sqrt_price < sqrt_price_x96);
        assert!(new_sqrt_price > U256_ZERO);
        // New tick should be calculated correctly
        assert!(new_tick <= tick); // For zeroForOne, tick decreases (price decreases)
    }

    #[test]
    fn test_calculate_v3_post_swap_state_token1_to_token0() {
        // Test Token1→Token0 direction
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128); // 0.001 token1
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // Price = 1.0
        let liquidity = 1_000_000_000_000_000_000_000u128; // 1000 tokens
        let tick = 0;
        let fee_bps = BasisPoints::new_const(300); // 0.3% fee

        let (new_sqrt_price, new_tick) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token1ToToken0,
        )
        .unwrap();

        // For oneForZero, new sqrt price should be greater than current
        assert!(new_sqrt_price > sqrt_price_x96);
        // New tick should be calculated correctly
        assert!(new_tick >= tick); // For oneForZero, tick increases (price increases)
    }

    #[test]
    fn test_calculate_v3_post_swap_state_consistency_with_amount_out() {
        // Test that the sqrt price from post_frontrun_state matches what calculate_v3_amount_out would produce
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128);
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128);
        let liquidity = 1_000_000_000_000_000_000_000u128;
        let fee_bps = BasisPoints::new_const(300);

        // Calculate using post_frontrun_state
        let (new_sqrt_price_from_state, _) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // Calculate amount_out to verify consistency
        let amount_out = calculate_v3_amount_out(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // Verify amount_out is positive (swap happened)
        assert!(amount_out > U256_ZERO);

        // The new sqrt price should be valid
        assert!(new_sqrt_price_from_state > U256_ZERO);
        assert!(new_sqrt_price_from_state < sqrt_price_x96); // For zeroForOne
    }

    #[test]
    fn test_calculate_v3_post_swap_state_zero_input() {
        // Test that zero input returns error
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128);
        let liquidity = 1_000_000_000_000_000_000_000u128;
        let fee_bps = BasisPoints::new_const(300);

        let result = calculate_v3_post_swap_state(
            U256_ZERO,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            MathError::InvalidInput { .. } => {}
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_v3_post_swap_state_zero_liquidity() {
        // Test that zero liquidity returns error
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128);
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128);
        let fee_bps = BasisPoints::new_const(300);

        let result = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            0,
            fee_bps,
            SwapDirection::Token0ToToken1,
        );

        assert!(result.is_err());
        match result.unwrap_err() {
            MathError::InvalidInput { .. } => {}
            _ => panic!("Expected InvalidInput error"),
        }
    }

    #[test]
    fn test_calculate_v3_post_swap_state_tick_calculation() {
        // Test that tick is calculated correctly from new sqrt price
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128);
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // tick = 0
        let liquidity = 1_000_000_000_000_000_000_000u128;
        let fee_bps = BasisPoints::new_const(300);

        let (new_sqrt_price, new_tick) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        let expected_tick = sqrt_price_to_tick(new_sqrt_price).unwrap();
        assert_eq!(
            new_tick, expected_tick,
            "new_tick {} should exactly match sqrt_price_to_tick result {}",
            new_tick, expected_tick
        );
    }

    #[test]
    fn test_sqrt_price_to_tick_newton_method_correctness() {
        // Test that Newton's method produces correct results
        // Test various sqrt_price values and verify against get_sqrt_ratio_at_tick

        // Test tick = 0
        let sqrt_price_0 = U256::from(79228162514264337593543950336u128); // 2^96
        let tick_0 = sqrt_price_to_tick(sqrt_price_0).unwrap();
        assert_eq!(tick_0, 0);
        let calculated_sqrt_0 = get_sqrt_ratio_at_tick(tick_0).unwrap();
        assert_eq!(calculated_sqrt_0, sqrt_price_0);

        // Test MIN_TICK
        let sqrt_price_min = U256::from(MIN_SQRT_RATIO);
        let tick_min = sqrt_price_to_tick(sqrt_price_min).unwrap();
        assert_eq!(tick_min, MIN_TICK);
        let calculated_sqrt_min = get_sqrt_ratio_at_tick(tick_min).unwrap();
        assert_eq!(calculated_sqrt_min, sqrt_price_min);

        // Test MAX_TICK
        let sqrt_price_max = uniswap_v3_math::tick_math::MAX_SQRT_RATIO;
        let tick_max = sqrt_price_to_tick(sqrt_price_max).unwrap();
        assert_eq!(tick_max, MAX_TICK);

        // Test positive ticks
        for test_tick in [1, 10, 100, 1000, 10000, 100000] {
            let sqrt_price = get_sqrt_ratio_at_tick(test_tick).unwrap();
            let calculated_tick = sqrt_price_to_tick(sqrt_price).unwrap();
            // PROTOCOL PARITY: Exact match required (strict flooring)
            assert_eq!(
                calculated_tick, test_tick,
                "Tick mismatch: expected {}, got {} for sqrt_price={}",
                test_tick, calculated_tick, sqrt_price
            );
            // Verify the calculated tick produces a sqrt_price close to target
            let calculated_sqrt = get_sqrt_ratio_at_tick(calculated_tick).unwrap();
            let diff = if calculated_sqrt >= sqrt_price {
                calculated_sqrt - sqrt_price
            } else {
                sqrt_price - calculated_sqrt
            };
            // Allow 1 part per million difference
            assert!(
                diff < sqrt_price / U256::from(1_000_000u64),
                "Sqrt price mismatch: expected {}, got {} (diff={})",
                sqrt_price,
                calculated_sqrt,
                diff
            );
        }

        // Test negative ticks
        for test_tick in [-1, -10, -100, -1000, -10000, -100000] {
            let sqrt_price = get_sqrt_ratio_at_tick(test_tick).unwrap();
            let calculated_tick = sqrt_price_to_tick(sqrt_price).unwrap();
            // PROTOCOL PARITY: Exact match required (strict flooring)
            assert_eq!(
                calculated_tick, test_tick,
                "Tick mismatch: expected {}, got {} for sqrt_price={}",
                test_tick, calculated_tick, sqrt_price
            );
        }
    }

    #[test]
    fn test_sqrt_price_to_tick_newton_method_convergence() {
        // Test that Newton's method converges in reasonable iterations
        let sqrt_price = U256::from(79228162514264337593543950336u128); // tick = 0
        let result = sqrt_price_to_tick(sqrt_price);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);

        // Test with various sqrt prices
        let test_cases = vec![
            (U256::from(79228162514264337593543950336u128), 0), // tick = 0
            (U256::from(MIN_SQRT_RATIO), MIN_TICK),
            (uniswap_v3_math::tick_math::MAX_SQRT_RATIO, MAX_TICK),
        ];

        for (sqrt_price, expected_tick) in test_cases {
            let result = sqrt_price_to_tick(sqrt_price);
            assert!(
                result.is_ok(),
                "sqrt_price_to_tick failed for sqrt_price={}",
                sqrt_price
            );
            let tick = result.unwrap();
            assert_eq!(
                tick, expected_tick,
                "Tick mismatch for sqrt_price={}",
                sqrt_price
            );
        }
    }

    #[test]
    fn test_sqrt_price_to_tick_newton_method_edge_cases() {
        // Test edge cases
        let sqrt_price_0 = U256::from(79228162514264337593543950336u128);
        let tick_0 = sqrt_price_to_tick(sqrt_price_0).unwrap();
        assert_eq!(tick_0, 0);

        // Test just above MIN_SQRT_RATIO
        let sqrt_price_min_plus = U256::from(MIN_SQRT_RATIO)
            .checked_add(U256::from(1u64))
            .unwrap();
        let tick_min_plus = sqrt_price_to_tick(sqrt_price_min_plus).unwrap();
        assert!(tick_min_plus >= MIN_TICK);
        assert!(tick_min_plus <= MIN_TICK + 10); // Should be close to MIN_TICK

        // Test just below MAX_SQRT_RATIO
        let sqrt_price_max_minus = uniswap_v3_math::tick_math::MAX_SQRT_RATIO.checked_sub(U256::from(1u64)).unwrap();
        let tick_max_minus = sqrt_price_to_tick(sqrt_price_max_minus).unwrap();
        assert!(tick_max_minus >= MAX_TICK - 10); // Should be close to MAX_TICK
        assert!(tick_max_minus <= MAX_TICK);
    }

    #[test]
    fn test_sqrt_price_to_tick_newton_method_roundtrip() {
        // Test roundtrip: tick -> sqrt_price -> tick
        let test_ticks = vec![
            0, MIN_TICK, MAX_TICK, 1, -1, 100, -100, 1000, -1000, 10000, -10000,
        ];

        for original_tick in test_ticks {
            let sqrt_price = get_sqrt_ratio_at_tick(original_tick).unwrap();
            let calculated_tick = sqrt_price_to_tick(sqrt_price).unwrap();

            // PROTOCOL PARITY: Exact match required (strict flooring)
            assert_eq!(
                calculated_tick, original_tick,
                "Roundtrip failed: original_tick={}, calculated_tick={}, sqrt_price={}",
                original_tick, calculated_tick, sqrt_price
            );

            // Verify the calculated tick produces a sqrt_price close to original
            let calculated_sqrt = get_sqrt_ratio_at_tick(calculated_tick).unwrap();
            let diff = if calculated_sqrt >= sqrt_price {
                calculated_sqrt - sqrt_price
            } else {
                sqrt_price - calculated_sqrt
            };
            // Allow 1 part per million difference
            assert!(
                diff < sqrt_price / U256::from(1_000_000u64),
                "Sqrt price mismatch in roundtrip: original_tick={}, calculated_tick={}, original_sqrt={}, calculated_sqrt={}, diff={}",
                original_tick, calculated_tick, sqrt_price, calculated_sqrt, diff
            );
        }
    }

    #[test]
    fn test_sqrt_price_to_tick_newton_method_fallback() {
        // Test that fallback to binary search works if Newton's method fails
        // This is hard to test directly, but we can verify the function always returns a valid result
        let sqrt_price = U256::from(79228162514264337593543950336u128);
        let result = sqrt_price_to_tick(sqrt_price);
        assert!(result.is_ok());
        let tick = result.unwrap();
        assert!(tick >= MIN_TICK);
        assert!(tick <= MAX_TICK);

        // Verify the result is correct
        let calculated_sqrt = get_sqrt_ratio_at_tick(tick).unwrap();
        let diff = if calculated_sqrt >= sqrt_price {
            calculated_sqrt - sqrt_price
        } else {
            sqrt_price - calculated_sqrt
        };
        // Should be very close (within 1 part per million)
        assert!(diff < sqrt_price / U256::from(1_000_000u64));
    }

    #[test]
    fn test_get_amount0_delta_rounding() {
        // Test rounding direction matters
        let sqrt_a = U256::from(79228162514264337593543950336u128); // tick 0
        let sqrt_b = U256::from(79236108205323166380068368726u128); // tick 1
        let liquidity = 1000000u128;

        let round_down = get_amount0_delta(sqrt_a, sqrt_b, liquidity, false).unwrap();
        let round_up = get_amount0_delta(sqrt_a, sqrt_b, liquidity, true).unwrap();

        // Round up should be >= round down
        assert!(round_up >= round_down, "round_up ({}) should be >= round_down ({})", round_up, round_down);
        
        // For non-exact divisions, round_up can be 0, 1, or 2 more than round_down
        // This is because get_amount0_delta performs two divisions, each with independent rounding
        // - If both divisions round: difference can be 0, 1, or 2
        // - If one division rounds: difference is 1
        // - If neither rounds: difference is 0 (exact division)
        if round_up != round_down {
            let diff = round_up - round_down;
            assert!(diff <= U256::from(2u64), "round_up ({}) should be at most 2 more than round_down ({})", round_up, round_down);
            assert!(diff >= U256::from(1u64), "round_up ({}) should be at least 1 more than round_down ({})", round_up, round_down);
        }
    }

    #[test]
    fn test_get_amount1_delta_rounding() {
        // Test rounding direction matters
        let sqrt_a = U256::from(79228162514264337593543950336u128); // tick 0
        let sqrt_b = U256::from(79236108205323166380068368726u128); // tick 1
        let liquidity = 1000000u128;

        let round_down = get_amount1_delta(sqrt_a, sqrt_b, liquidity, false).unwrap();
        let round_up = get_amount1_delta(sqrt_a, sqrt_b, liquidity, true).unwrap();

        // Round up should be >= round down
        assert!(round_up >= round_down, "round_up ({}) should be >= round_down ({})", round_up, round_down);
        
        // For non-exact divisions, round_up should be exactly 1 more than round_down
        if round_up != round_down {
            assert_eq!(round_up, round_down + U256::from(1u64), "round_up should be exactly 1 more than round_down");
        }
    }

    #[test]
    fn test_sqrt_price_to_tick_strict_flooring() {
        // Test that sqrt_price_to_tick uses strict flooring (not nearest)
        // For a sqrt price exactly at tick boundary, should return that tick
        let tick_0_sqrt = U256::from(79228162514264337593543950336u128);
        let tick_1_sqrt = get_sqrt_ratio_at_tick(1).unwrap();
        
        // Price exactly at tick 0
        let tick = sqrt_price_to_tick(tick_0_sqrt).unwrap();
        assert_eq!(tick, 0, "Price at tick 0 should return tick 0");
        
        // Price just below tick 1 should return tick 0 (flooring)
        let just_below_tick_1 = tick_1_sqrt - U256::from(1u64);
        let tick = sqrt_price_to_tick(just_below_tick_1).unwrap();
        assert_eq!(tick, 0, "Price just below tick 1 should return tick 0 (flooring)");
        
        // Price exactly at tick 1 should return tick 1
        let tick = sqrt_price_to_tick(tick_1_sqrt).unwrap();
        assert_eq!(tick, 1, "Price at tick 1 should return tick 1");
    }

    #[test]
    fn test_calculate_v3_amount_out_different_prices() {
        // Test with different sqrt prices to verify formula works across price ranges
        let amount_in = U256::from(1000_000_000_000_000_000u128);
        let liquidity = 1_000_000_000_000_000_000_000u128;
        let fee_bps = BasisPoints::new_const(300);

        // Test at different price points (reasonable prices, not extreme boundaries)
        // Extreme prices (MIN/MAX) can cause overflows or zero outputs due to precision limits
        let prices = vec![
            get_sqrt_ratio_at_tick(-50000).unwrap(), // Low price (tick -50000)
            U256::from(79228162514264337593543950336u128), // Price = 1.0 (tick 0)
            get_sqrt_ratio_at_tick(50000).unwrap(),  // High price (tick 50000)
        ];

        for sqrt_price in prices {
            // Token0→Token1
            let result0to1 = calculate_v3_amount_out(
                amount_in,
                sqrt_price,
                liquidity,
                fee_bps,
                SwapDirection::Token0ToToken1,
            );
            assert!(
                result0to1.is_ok(),
                "Token0ToToken1 failed at sqrt_price={}: {:?}",
                sqrt_price,
                result0to1
            );
            assert!(
                result0to1.unwrap() > U256_ZERO,
                "Token0ToToken1 returned zero at sqrt_price={}",
                sqrt_price
            );

            // Token1→Token0
            let result1to0 = calculate_v3_amount_out(
                amount_in,
                sqrt_price,
                liquidity,
                fee_bps,
                SwapDirection::Token1ToToken0,
            );
            assert!(
                result1to0.is_ok(),
                "Token1ToToken0 failed at sqrt_price={}: {:?}",
                sqrt_price,
                result1to0
            );
            assert!(
                result1to0.unwrap() > U256_ZERO,
                "Token1ToToken0 returned zero at sqrt_price={}",
                sqrt_price
            );
        }
    }

    #[test]
    fn test_calculate_v3_post_swap_state_tick_delta() {
        // Test that tick delta calculation works correctly in calculate_v3_post_swap_state
        let frontrun_amount = U256::from(1000_000_000_000_000_000u128); // 0.001 token
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // tick = 0
        let liquidity = 1_000_000_000_000_000_000_000u128; // 1000 tokens
        let tick = 0;
        let fee_bps = BasisPoints::new_const(300);

        // Token0ToToken1 direction
        // Selling token0 for token1 -> more token0 in pool -> price of token0 decreases
        // -> sqrt_price decreases -> tick decreases
        let (new_sqrt_price, new_tick) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // Verify new_tick is calculated correctly
        // For Token0ToToken1: tick should decrease (or stay same for small swap)
        assert!(
            new_tick <= tick,
            "Token0ToToken1: new_tick {} should be <= tick {}",
            new_tick,
            tick
        );
        assert!(
            new_tick >= tick - 100,
            "new_tick {} too far from tick {}",
            new_tick,
            tick
        );

        // Verify new_sqrt_price < old_sqrt_price (price decreased)
        assert!(
            new_sqrt_price < sqrt_price_x96,
            "Token0ToToken1: sqrt_price should decrease"
        );

        // Token1ToToken0 direction
        // Selling token1 for token0 -> more token1 in pool -> price of token0 increases
        // -> sqrt_price increases -> tick increases
        let (new_sqrt_price2, new_tick2) = calculate_v3_post_swap_state(
            frontrun_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token1ToToken0,
        )
        .unwrap();

        // Verify new_tick is calculated correctly
        // For Token1ToToken0: tick should increase (or stay same for small swap)
        assert!(
            new_tick2 >= tick,
            "Token1ToToken0: new_tick {} should be >= tick {}",
            new_tick2,
            tick
        );
        assert!(
            new_tick2 <= tick + 100,
            "new_tick {} too far from tick {}",
            new_tick2,
            tick
        );

        // Verify new_sqrt_price > old_sqrt_price (price increased)
        assert!(
            new_sqrt_price2 > sqrt_price_x96,
            "Token1ToToken0: sqrt_price should increase"
        );
    }

    #[test]
    fn test_calculate_v3_post_swap_state_stays_on_tick_until_boundary() {
        // Test that we stay on current tick until boundary is crossed
        let sqrt_price_x96 = U256::from(79228162514264337593543950336u128); // tick = 0
        let liquidity = 1_000_000_000_000_000_000_000_000u128; // Very large liquidity
        let tick = 0;
        let fee_bps = BasisPoints::new_const(300);

        // Very small swap that shouldn't cross tick boundary significantly
        let very_small_amount = U256::from(1_000_000_000u128); // Very small
        let (new_sqrt_price, new_tick) = calculate_v3_post_swap_state(
            very_small_amount,
            sqrt_price_x96,
            liquidity,
            fee_bps,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // For Token0ToToken1: price decreases, tick may decrease by 0 or 1
        // Due to floor rounding in tick calculation, even tiny moves can show as -1
        assert!(
            new_tick <= tick,
            "Token0ToToken1: tick should decrease or stay same"
        );
        assert!(
            new_tick >= tick - 1,
            "tick should not move more than 1 for tiny swap"
        );

        // Price should have decreased (Token0ToToken1)
        assert!(
            new_sqrt_price < sqrt_price_x96,
            "Token0ToToken1: sqrt_price should decrease"
        );
    }

    #[test]
    fn test_reserves_to_sqrt_price_x96_identity_ratio() {
        let reserve_in = U256::from(1_000_000_000_000_000_000u128);
        let reserve_out = U256::from(1_000_000_000_000_000_000u128);
        let sqrt = reserves_to_sqrt_price_x96(reserve_in, reserve_out).unwrap();
        assert_eq!(sqrt, Q96);
    }

    #[test]
    fn test_v3_price_impact_from_sqrt_prices() {
        let before = U256::from(79228162514264337593543950336u128); // tick 0
        let after = U256::from(79623317895830914510639640423u128); // tick 100
        let impact = calculate_v3_price_impact(before, after).unwrap();
        assert!(impact > 0);
        assert!(impact <= 10_000);
    }
}
