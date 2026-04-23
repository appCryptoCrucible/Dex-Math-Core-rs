//! Kyber Elastic Core Mathematics
//!
//! This module implements Kyber Elastic's core mathematical functions for
//! concentrated liquidity AMM calculations. Kyber Elastic uses tick-based
//! pricing similar to Uniswap V3 but with different mathematical formulas.
//!
//! Key differences from Uniswap V3:
//! - Different tick spacing and range calculations
//! - Unique swap step calculations with fee handling
//! - Custom liquidity math
//! - Reinvestment token mechanics

use crate::core::MathError;
use alloy_primitives::{I256, U256};
use uniswap_v3_math;

const Q96: U256 = U256::from_limbs([0, 0x100000000, 0, 0]); // 1 << 96

#[inline(always)]
fn low_u128(v: U256) -> u128 {
    let limbs = v.as_limbs();
    limbs[0] as u128 | ((limbs[1] as u128) << 64)
}

/// Kyber TickMath - Core tick to price conversions
pub mod tick_math {
    use super::*;

    /// Minimum tick value for Kyber Elastic (same as Uniswap V3)
    /// Corresponds to sqrt(1.0001^MIN_TICK) in Q64.96 format
    pub const MIN_TICK: i32 = -887272;

    /// Maximum tick value for Kyber Elastic (same as Uniswap V3)
    /// Corresponds to sqrt(1.0001^MAX_TICK) in Q64.96 format
    pub const MAX_TICK: i32 = 887272;

    /// Minimum square root ratio in Q64.96 format (canonical Uniswap V3 / Kyber Elastic)
    pub const MIN_SQRT_RATIO: U256 = super::uniswap_v3_math::tick_math::MIN_SQRT_RATIO;

    /// Maximum square root ratio in Q64.96 format (canonical Uniswap V3 / Kyber Elastic)
    pub const MAX_SQRT_RATIO: U256 = super::uniswap_v3_math::tick_math::MAX_SQRT_RATIO;

    #[inline(always)]
    pub fn get_max_sqrt_ratio() -> U256 {
        MAX_SQRT_RATIO
    }

    /// Convert tick to square root price ratio
    /// Production-grade implementation matching Uniswap V3 TickMath.sol
    ///
    /// # Formula
    /// sqrt_price = sqrt(1.0001^tick) * 2^96
    ///
    /// # Arguments
    /// * `tick` - The tick value in range [MIN_TICK, MAX_TICK]
    ///
    /// # Returns
    /// * `Ok(U256)` - Sqrt price in Q64.96 format
    /// * `Err(MathError)` - If tick is out of valid range
    #[inline(always)]
    pub fn get_sqrt_ratio_at_tick(tick: i32) -> Result<U256, MathError> {
        if tick < MIN_TICK || tick > MAX_TICK {
            return Err(MathError::InvalidInput {
                operation: "get_sqrt_ratio_at_tick".to_string(),
                reason: format!("Tick {} out of bounds [{}, {}]", tick, MIN_TICK, MAX_TICK),
                context: "Kyber TickMath".to_string(),
            });
        }

        super::uniswap_v3_math::tick_math::get_sqrt_ratio_at_tick(tick).map_err(|e| {
            MathError::InvalidInput {
                operation: "get_sqrt_ratio_at_tick".to_string(),
                reason: format!("{}", e),
                context: "uniswap_v3_math crate".to_string(),
            }
        })
    }

    /// Convert square root price ratio to tick (delegates to canonical `uniswap_v3_math` TickMath).
    ///
    /// # Arguments
    /// * `sqrt_price_x96` - Sqrt price in Q64.96 format
    ///
    /// # Returns
    /// * `Ok(i32)` - The tick corresponding to the sqrt price
    /// * `Err(MathError)` - If sqrt price is out of valid range
    #[inline(always)]
    pub fn get_tick_at_sqrt_ratio(sqrt_price_x96: U256) -> Result<i32, MathError> {
        if sqrt_price_x96 < MIN_SQRT_RATIO || sqrt_price_x96 >= MAX_SQRT_RATIO {
            return Err(MathError::InvalidInput {
                operation: "get_tick_at_sqrt_ratio".to_string(),
                reason: format!("sqrt_ratio {} out of range", sqrt_price_x96),
                context: "Kyber tick math".to_string(),
            });
        }
        super::uniswap_v3_math::tick_math::get_tick_at_sqrt_ratio(sqrt_price_x96).map_err(|e| {
            MathError::InvalidInput {
                operation: "get_tick_at_sqrt_ratio".to_string(),
                reason: format!("{}", e),
                context: "uniswap_v3_math crate".to_string(),
            }
        })
    }

    // get_sqrt_ratio_at_tick/get_tick_at_sqrt_ratio delegate to canonical TickMath.
}

/// Kyber SwapMath - Swap step calculations
pub mod swap_math {
    use super::*;
    use uniswap_v3_math::swap_math as canonical_swap_math;

    /// Result of a swap step calculation
    #[derive(Debug, Clone)]
    pub struct SwapStepResult {
        pub used_amount: i128,
        pub returned_amount: i128,
        pub delta_l: u128,
        pub next_sqrt_p: U256,
    }

    #[inline(always)]
    fn u256_to_i128_checked(v: U256, operation: &str, context: &str) -> Result<i128, MathError> {
        let max_i128 = U256::from(i128::MAX as u128);
        if v > max_i128 {
            return Err(MathError::InvalidInput {
                operation: operation.to_string(),
                reason: "value exceeds i128 range".to_string(),
                context: format!("{}; value={}", context, v),
            });
        }
        let limbs = v.as_limbs();
        let raw = limbs[0] as u128 | ((limbs[1] as u128) << 64);
        i128::try_from(raw).map_err(|_| MathError::InvalidInput {
            operation: operation.to_string(),
            reason: "u256->i128 conversion failed".to_string(),
            context: format!("{}; value={}", context, v),
        })
    }

    /// Compute a single Kyber swap step.
    ///
    /// Exact input and exact output both follow canonical SwapMath semantics.
    /// - `is_exact_input = true`: `specified_amount` is max input (gross, incl. fee)
    /// - `is_exact_input = false`: `specified_amount` is desired output
    #[inline(always)]
    pub fn compute_swap_step(
        liquidity: u128,
        current_sqrt_p: U256,
        target_sqrt_p: U256,
        fee_in_bps: u32,
        specified_amount: i128,
        is_exact_input: bool,
        _is_token0: bool,
    ) -> Result<SwapStepResult, MathError> {
        if liquidity == 0 {
            return Err(MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: "liquidity cannot be zero".to_string(),
                context: "kyber swap step".to_string(),
            });
        }
        if current_sqrt_p.is_zero() || target_sqrt_p.is_zero() {
            return Err(MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: "sqrt prices must be non-zero".to_string(),
                context: format!("current={}, target={}", current_sqrt_p, target_sqrt_p),
            });
        }
        if fee_in_bps >= 10_000 {
            return Err(MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: "fee_in_bps must be < 10000".to_string(),
                context: format!("fee_in_bps={}", fee_in_bps),
            });
        }
        let abs_specified = specified_amount.checked_abs().ok_or_else(|| MathError::InvalidInput {
            operation: "compute_swap_step".to_string(),
            reason: "specified_amount overflow on abs()".to_string(),
            context: format!("specified_amount={}", specified_amount),
        })?;
        if abs_specified == 0 {
            return Ok(SwapStepResult {
                used_amount: 0,
                returned_amount: 0,
                delta_l: 0,
                next_sqrt_p: current_sqrt_p,
            });
        }

        // Uniswap fee pips are 1e-6; Kyber bps are 1e-4.
        let fee_pips = fee_in_bps.checked_mul(100).ok_or_else(|| MathError::Overflow {
            operation: "compute_swap_step".to_string(),
            inputs: vec![],
            context: "fee bps -> fee pips".to_string(),
        })?;
        let amount_abs_i256 = I256::from_raw(U256::from(abs_specified as u128));
        let amount_remaining = if is_exact_input {
            amount_abs_i256
        } else {
            -amount_abs_i256
        };

        let (next_sqrt_p, amount_in_net, amount_out, fee_amount) =
            canonical_swap_math::compute_swap_step(
                current_sqrt_p,
                target_sqrt_p,
                liquidity,
                amount_remaining,
                fee_pips,
            )
            .map_err(|e| MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: format!("{}", e),
                context: "canonical swap step".to_string(),
            })?;

        let total_input = amount_in_net
            .checked_add(fee_amount)
            .ok_or_else(|| MathError::Overflow {
                operation: "compute_swap_step".to_string(),
                inputs: vec![],
                context: "amount_in + fee".to_string(),
            })?;
        let used_amount = u256_to_i128_checked(total_input, "compute_swap_step", "total input")?;
        let amount_out_i128 = u256_to_i128_checked(amount_out, "compute_swap_step", "amount out")?;
        let returned_amount = -amount_out_i128;
        let delta_l = low_u128(fee_amount);

        if is_exact_input && used_amount > abs_specified {
            return Err(MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: "used input exceeds specified exact-input amount".to_string(),
                context: format!("used={}, specified={}", used_amount, abs_specified),
            });
        }
        if !is_exact_input && amount_out_i128 > abs_specified {
            return Err(MathError::InvalidInput {
                operation: "compute_swap_step".to_string(),
                reason: "output exceeds specified exact-output amount".to_string(),
                context: format!("out={}, specified={}", amount_out_i128, abs_specified),
            });
        }

        Ok(SwapStepResult {
            used_amount,
            returned_amount,
            delta_l,
            next_sqrt_p,
        })
    }

    /// Calculate reach amount for a given liquidity and price bounds
    /// Based on Kyber/Uniswap V3 swap math formulas
    ///
    /// For token0 -> token1 (price decreasing): amount = L * (sqrt_p_current - sqrt_p_target) / (sqrt_p_current * sqrt_p_target / 2^96)
    /// For token1 -> token0 (price increasing): amount = L * (sqrt_p_target - sqrt_p_current)
    #[inline(always)]
    pub fn calc_reach_amount(
        liquidity: u128,
        current_sqrt_p: U256,
        target_sqrt_p: U256,
        fee_in_bps: u32,
        is_exact_input: bool,
        is_token0: bool,
    ) -> Result<i128, MathError> {
        if fee_in_bps >= 10_000 {
            return Err(MathError::InvalidInput {
                operation: "calc_reach_amount".to_string(),
                reason: "fee_in_bps must be < 10000".to_string(),
                context: format!("fee_in_bps={}", fee_in_bps),
            });
        }
        if current_sqrt_p.is_zero() || target_sqrt_p.is_zero() {
            return Err(MathError::InvalidInput {
                operation: "calc_reach_amount".to_string(),
                reason: "sqrt prices must be non-zero".to_string(),
                context: format!("current={}, target={}", current_sqrt_p, target_sqrt_p),
            });
        }

        #[inline(always)]
        fn u256_to_u128_checked(v: U256, operation: &str, context: &str) -> Result<u128, MathError> {
            let limbs = v.as_limbs();
            if limbs[2] != 0 || limbs[3] != 0 {
                return Err(MathError::Overflow {
                    operation: operation.to_string(),
                    inputs: vec![],
                    context: format!("{}; value={}", context, v),
                });
            }
            Ok(limbs[0] as u128 | ((limbs[1] as u128) << 64))
        }

        let liquidity_u256 = U256::from(liquidity);

        // Determine price direction
        let (high_price, low_price) = if target_sqrt_p > current_sqrt_p {
            (target_sqrt_p, current_sqrt_p)
        } else {
            (current_sqrt_p, target_sqrt_p)
        };

        let price_diff = high_price - low_price;

        let mut amount = if is_token0 {
            // Token0 amount formula: amount0 = L * (sqrt_P_upper - sqrt_P_lower) / (sqrt_P_upper * sqrt_P_lower)
            // In Q96: amount0 = L * Q96 * (sqrt_P_upper - sqrt_P_lower) / (sqrt_P_upper * sqrt_P_lower)

            let numerator = liquidity_u256
                .checked_mul(Q96)
                .and_then(|v| v.checked_mul(price_diff))
                .ok_or_else(|| MathError::Overflow {
                    operation: "calc_reach_amount".to_string(),
                    inputs: vec![],
                    context: "liquidity * Q96 * price_diff".to_string(),
                })?;

            // Denominator: sqrt_P_upper * sqrt_P_lower
            // This is very large (Q192), so we need careful division
            let denominator = high_price
                .checked_mul(low_price)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calc_reach_amount".to_string(),
                    inputs: vec![],
                    context: "high_price * low_price".to_string(),
                })?
                / Q96;

            if denominator.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "calc_reach_amount".to_string(),
                    context: "denominator for token0 path".to_string(),
                });
            } else {
                u256_to_u128_checked(numerator / denominator, "calc_reach_amount", "token0 amount")
            }
        } else {
            // Token1 amount formula: amount1 = L * (sqrt_P_upper - sqrt_P_lower) / Q96
            let amount_scaled = liquidity_u256
                .checked_mul(price_diff)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calc_reach_amount".to_string(),
                    inputs: vec![],
                    context: "liquidity * price_diff".to_string(),
                })?
                / Q96;
            u256_to_u128_checked(amount_scaled, "calc_reach_amount", "token1 amount")
        };

        // Exact input: `amount` is net liquidity delta; gross input must satisfy
        // gross * (10000 - fee) / 10000 = net.
        if is_exact_input {
            let denom = U256::from((10000u32 - fee_in_bps) as u64);
            amount = u256_to_u128_checked(
                U256::from(amount?)
                    .checked_mul(U256::from(10000u64))
                    .ok_or_else(|| MathError::Overflow {
                        operation: "calc_reach_amount".to_string(),
                        inputs: vec![],
                        context: "gross-up multiply by 10000".to_string(),
                    })?
                    / denom,
                "calc_reach_amount",
                "grossed amount",
            );
        }

        let amount = amount?;
        if is_exact_input {
            Ok(amount as i128)
        } else {
            Ok(-(amount as i128))
        }
    }
}

/// Kyber LiqDeltaMath - Liquidity delta operations
pub mod liq_delta_math {
    use crate::core::MathError;
    use crate::dex::common::alloy_to_ethers;
    use alloy_primitives::U256;

    /// Apply liquidity delta to current liquidity
    /// Based on Kyber's LiqDeltaMath.applyLiquidityDelta()
    ///
    /// # Arguments
    /// * `current_liquidity` - Current pool liquidity
    /// * `liquidity_delta` - Amount to add (positive) or remove (negative)
    /// * `is_add_liquidity` - True if adding liquidity, false if removing
    ///
    /// # Returns
    /// * `Ok(u128)` - New liquidity after applying delta
    /// * `Err(MathError)` - If operation is invalid or would underflow
    #[inline(always)]
    pub fn apply_liquidity_delta(
        current_liquidity: u128,
        liquidity_delta: i128,
        is_add_liquidity: bool,
    ) -> Result<u128, MathError> {
        if liquidity_delta == 0 {
            return Ok(current_liquidity);
        }

        let delta_abs = liquidity_delta.unsigned_abs();
        if is_add_liquidity {
            if liquidity_delta < 0 {
                return Err(MathError::InvalidInput {
                    operation: "apply_liquidity_delta".to_string(),
                    reason: "Liquidity delta sign must match operation direction".to_string(),
                    context: format!("is_add={}, delta={}", is_add_liquidity, liquidity_delta),
                });
            }
            current_liquidity
                .checked_add(delta_abs)
                .ok_or_else(|| MathError::Overflow {
                    operation: "apply_liquidity_delta".to_string(),
                    inputs: vec![
                        alloy_to_ethers(U256::from(current_liquidity)),
                        alloy_to_ethers(U256::from(delta_abs)),
                    ],
                    context: "Adding liquidity would overflow u128".to_string(),
                })
        } else {
            if liquidity_delta > 0 {
                return Err(MathError::InvalidInput {
                    operation: "apply_liquidity_delta".to_string(),
                    reason: "Liquidity delta sign must match operation direction".to_string(),
                    context: format!("is_add={}, delta={}", is_add_liquidity, liquidity_delta),
                });
            }
            current_liquidity
                .checked_sub(delta_abs)
                .ok_or_else(|| MathError::Underflow {
                    operation: "apply_liquidity_delta".to_string(),
                    inputs: vec![
                        alloy_to_ethers(U256::from(current_liquidity)),
                        alloy_to_ethers(U256::from(delta_abs)),
                    ],
                    context: "Insufficient liquidity for removal".to_string(),
                })
        }
    }
}

// TODO: Re-enable these tests after completing the tick_math module refactoring
// #[cfg(test)]
// mod tests {
//
//     #[test]
//     fn test_tick_math_bounds() {
//         // Test min tick
//         let min_ratio = tick_math::get_sqrt_ratio_at_tick(tick_math::MIN_TICK).unwrap();
//         assert_eq!(min_ratio, tick_math::MIN_SQRT_RATIO);
//
//         // Test max tick
//         let max_ratio = tick_math::get_sqrt_ratio_at_tick(tick_math::MAX_TICK).unwrap();
//         assert_eq!(max_ratio, tick_math::MAX_SQRT_RATIO);
//
//         // Test tick 0
//         let zero_ratio = tick_math::get_sqrt_ratio_at_tick(0).unwrap();
//         assert_eq!(zero_ratio, U256::from(1u128) << 96);
//     }
//
//     #[test]
//     fn test_tick_round_trip() {
//         let test_ticks = [-100, -10, -1, 0, 1, 10, 100, 1000, 5000, 10000];
//
//         for tick in test_ticks {
//             if tick >= tick_math::MIN_TICK && tick <= tick_math::MAX_TICK {
//                 let ratio = tick_math::get_sqrt_ratio_at_tick(tick).unwrap();
//                 let recovered_tick = tick_math::get_tick_at_sqrt_ratio(ratio).unwrap();
//
//                 // Allow for small rounding differences
//                 assert!((recovered_tick - tick).abs() <= 1,
//                        "Tick round-trip failed: {} -> {} -> {}", tick, ratio, recovered_tick);
//             }
//         }
//     }
// }

#[cfg(test)]
mod parity_tests {
    use super::swap_math::compute_swap_step;
    use alloy_primitives::U256;

    #[test]
    fn exact_output_is_capped_to_requested_amount() {
        let sqrt_p = U256::from(79228162514264337593543950336u128); // tick 0
        let target = U256::from(79623317895830914510639640423u128); // higher target
        let liquidity = 2_000_000_000_000_000_000u128;
        let fee_bps = 6; // 0.06%
        let requested_out = 1_000_000_000_000_000u128;

        let step = compute_swap_step(
            liquidity,
            sqrt_p,
            target,
            fee_bps,
            requested_out as i128,
            false, // exact output
            false,
        )
        .unwrap();

        let out = step.returned_amount.unsigned_abs();
        assert!(out <= requested_out);
        assert!(step.used_amount >= 0);
    }

    #[test]
    fn exact_input_consumes_no_more_than_specified() {
        let sqrt_p = U256::from(79228162514264337593543950336u128);
        let target = U256::from(79623317895830914510639640423u128);
        let liquidity = 2_000_000_000_000_000_000u128;
        let fee_bps = 6; // 0.06%
        let specified_in = 1_000_000_000_000_000u128;

        let step = compute_swap_step(
            liquidity,
            sqrt_p,
            target,
            fee_bps,
            specified_in as i128,
            true, // exact input
            false,
        )
        .unwrap();

        assert!(step.used_amount >= 0);
        assert!((step.used_amount as u128) <= specified_in);
        assert!(step.returned_amount <= 0);
    }
}
