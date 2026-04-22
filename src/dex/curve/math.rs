//! Curve Finance StableSwap Mathematics
//!
//! This module implements Curve Finance's StableSwap invariant and exchange
//! functions for stablecoin and pegged asset pools. Curve uses a modified
//! constant sum invariant that allows for efficient stablecoin swaps with
//! low slippage.
//!
//! ## Fee Application
//!
//! Fees are applied to the **raw** output amount in `calculate_dy()`, matching
//! Curve Vyper `get_dy` (`StableSwap3Pool.vy`): `_fee = fee * dy / FEE_DENOMINATOR`,
//! `FEE_DENOMINATOR = 1e10`, and `Ann = amp * N_COINS` in `get_D` / `get_y`.
//!
//! Key formulas:
//! - `xp[i] = balance[i] * RATES[i] / LENDING_PRECISION` with `RATES[i] = 10^(36 - decimals[i])`
//!   (matches 3pool DAI/USDC/USDT when `decimals` are 18/6/6).
//! - `dx` to `xp`: `dx_xp = dx_raw * RATES[i] / PRECISION` (`PRECISION = 1e18`).
//! - **Vyper 0.2** (`StableSwap3Pool`): `dy_raw = (xp[j] - y - 1) * PRECISION / RATES[j]`.
//! - **Vyper 0.1 legacy** (`StableSwapSUSD`, `StableSwapUSDT`): `D_P` uses divisor `x * N + 1`; `dy_raw = (xp[j] - y) * PRECISION / rates[j]` (no −1).
//! - Newton's method: Used for solving the invariant equation

use crate::core::MathError;
use crate::data::curve_registry::StableswapMathVariant;
use alloy_primitives::U256;
use crate::dex::common::alloy_to_ethers;

const ZERO: U256 = U256::ZERO;
const ONE: U256 = U256::from_limbs([1, 0, 0, 0]);
const TWO: U256 = U256::from_limbs([2, 0, 0, 0]);
const THREE: U256 = U256::from_limbs([3, 0, 0, 0]);
const FOUR: U256 = U256::from_limbs([4, 0, 0, 0]);
const BPS_DENOM: U256 = U256::from_limbs([10_000, 0, 0, 0]);
const WAD: U256 = U256::from_limbs([1_000_000_000_000_000_000, 0, 0, 0]);
/// Curve `LENDING_PRECISION` / `PRECISION` in `StableSwap3Pool.vy`.
const CURVE_LENDING_PRECISION: U256 = WAD;
const CURVE_PRECISION: U256 = WAD;
const CURVE_FEE_DENOMINATOR: U256 = U256::from_limbs([10_000_000_000u64, 0, 0, 0]);
const TEST_AMOUNT: U256 = U256::from_limbs([1_000_000, 0, 0, 0]);
/// Max ERC-20 decimals supported for embedded `RATES` derivation (`10^(36 - dec)`).
const MAX_STABLESWAP_DECIMALS: u8 = 18;

#[inline]
fn pow10_u256(exp: u32) -> Result<U256, MathError> {
    let mut acc = ONE;
    let ten = U256::from(10u64);
    for _ in 0..exp {
        acc = acc.checked_mul(ten).ok_or_else(|| MathError::Overflow {
            operation: "pow10_u256".to_string(),
            inputs: vec![alloy_to_ethers(acc), alloy_to_ethers(ten)],
            context: format!("10^{}", exp),
        })?;
    }
    Ok(acc)
}

/// StableSwap `RATES[i]` matching 3pool when `xp = balance * RATE / 1e18` (Vyper `_xp`).
#[inline]
pub fn stableswap_rate_for_decimals(decimals: u8) -> Result<U256, MathError> {
    if decimals > MAX_STABLESWAP_DECIMALS {
        return Err(MathError::InvalidInput {
            operation: "stableswap_rate_for_decimals".to_string(),
            reason: format!("decimals {} > {}", decimals, MAX_STABLESWAP_DECIMALS),
            context: "Curve stableswap RATES derivation".to_string(),
        });
    }
    let exp = (MAX_STABLESWAP_DECIMALS as u32 + 18u32).saturating_sub(decimals as u32);
    pow10_u256(exp)
}

/// `xp[i] = balances[i] * rates[i] / 1e18` for both3pool-style and legacy `_stored_rates()` pools.
pub fn stableswap_xp_from_rates(balances: &[U256], rates: &[U256]) -> Result<Vec<U256>, MathError> {
    if balances.len() != rates.len() {
        return Err(MathError::InvalidInput {
            operation: "stableswap_xp_from_rates".to_string(),
            reason: format!(
                "balances len {} != rates len {}",
                balances.len(),
                rates.len()
            ),
            context: "".to_string(),
        });
    }
    let mut xp = Vec::with_capacity(balances.len());
    for (&b, &r) in balances.iter().zip(rates.iter()) {
        let num = b.checked_mul(r).ok_or_else(|| MathError::Overflow {
            operation: "stableswap_xp_from_rates".to_string(),
            inputs: vec![alloy_to_ethers(b), alloy_to_ethers(r)],
            context: "balance * rate".to_string(),
        })?;
        let x = num.checked_div(CURVE_LENDING_PRECISION).ok_or_else(|| MathError::DivisionByZero {
            operation: "stableswap_xp_from_rates".to_string(),
            context: "balance * rate / 1e18".to_string(),
        })?;
        xp.push(x);
    }
    Ok(xp)
}

/// Use on-chain `_stored_rates()` when `Some`; otherwise derive3pool-style rates from `decimals`.
pub fn stableswap_rates_resolve(
    decimals: &[u8],
    stored_rates: Option<&[U256]>,
) -> Result<Vec<U256>, MathError> {
    if let Some(r) = stored_rates {
        if r.len() != decimals.len() {
            return Err(MathError::InvalidInput {
                operation: "stableswap_rates_resolve".to_string(),
                reason: format!(
                    "stored_rates len {} != decimals len {}",
                    r.len(),
                    decimals.len()
                ),
                context: "".to_string(),
            });
        }
        return Ok(r.to_vec());
    }
    let mut out = Vec::with_capacity(decimals.len());
    for &d in decimals {
        out.push(stableswap_rate_for_decimals(d)?);
    }
    Ok(out)
}

/// `xp` for **Vyper 0.2** pools where `RATES[i] = 10^(36 - decimals[i])`.
pub fn stableswap_xp_from_balances(
    balances: &[U256],
    decimals: &[u8],
) -> Result<Vec<U256>, MathError> {
    let rates = stableswap_rates_resolve(decimals, None)?;
    stableswap_xp_from_rates(balances, &rates)
}

#[inline]
fn curve_fee_raw_from_bps(fee_bps: u32) -> U256 {
    U256::from(fee_bps as u64).saturating_mul(U256::from(1_000_000u64))
}

#[inline]
fn curve_fee_scalar(fee_raw: U256, fee_bps: u32) -> U256 {
    if fee_raw.is_zero() {
        curve_fee_raw_from_bps(fee_bps)
    } else {
        fee_raw
    }
}

/// Calculate the Curve invariant D using Newton's method.
///
/// Uses Curve's production algorithm: D_P is built iteratively instead of
/// computing D^(n+1) directly (overflow-safe).
///
/// Algorithm from Curve's StableSwap:
/// ```text
/// D_P = D
/// for x in xp:
///     D_P = D_P * D / (x * N)
/// D = (Ann * S + D_P * N) * D / ((Ann - 1) * D + (N + 1) * D_P)
/// ```
///
/// # Arguments
/// * `balances` - Array of token balances in the pool (18-decimal scaled)
/// * `a` - Amplification coefficient (typically 100-1000)
/// * `n` - Number of tokens in the pool
///
/// # Returns
/// * `Ok(U256)` - The invariant D value
/// * `Err(MathError)` - Calculation error
pub fn calculate_d(
    balances: &[U256],
    a: U256,
    n: usize,
    variant: StableswapMathVariant,
) -> Result<U256, MathError> {
    if balances.len() != n {
        return Err(MathError::InvalidInput {
            operation: "calculate_d".to_string(),
            reason: format!("Balance count {} doesn't match n {}", balances.len(), n),
            context: "".to_string(),
        });
    }

    if n == 0 {
        return Err(MathError::InvalidInput {
            operation: "calculate_d".to_string(),
            reason: "Pool must have at least 1 token".to_string(),
            context: "".to_string(),
        });
    }

    let sum_x: U256 = balances
        .iter()
        .fold(ZERO, |acc, &x| acc.saturating_add(x));
    if sum_x == ZERO {
        return Ok(ZERO);
    }

    for balance in balances.iter() {
        if *balance == ZERO {
            return Ok(ZERO);
        }
    }

    let n_u256 = U256::from(n as u64);

    // Vyper `get_D`: `Ann = amp * N_COINS` (not A * n^n).
    let ann = a.checked_mul(n_u256).ok_or_else(|| MathError::Overflow {
        operation: "calculate_d".to_string(),
        inputs: vec![alloy_to_ethers(a), alloy_to_ethers(n_u256)],
        context: "Ann = A * N_COINS".to_string(),
    })?;

    let ann_minus_1 = ann.saturating_sub(ONE);
    let n_plus_1 = n_u256.saturating_add(ONE);

    let mut balance_times_n_stack = [ZERO; 4];
    let mut balance_times_n_vec: Vec<U256> = Vec::new();
    let balance_times_n: &[U256] = if n <= 4 {
        for i in 0..n {
            balance_times_n_stack[i] =
                balances[i]
                    .checked_mul(n_u256)
                    .ok_or_else(|| MathError::Overflow {
                        operation: "calculate_d".to_string(),
                        inputs: vec![alloy_to_ethers(balances[i]), alloy_to_ethers(n_u256)],
                        context: "balance * n (hoisted for D_P)".to_string(),
                    })?;
        }
        &balance_times_n_stack[..n]
    } else {
        balance_times_n_vec.reserve(n);
        for &balance in balances {
            balance_times_n_vec.push(
                balance
                    .checked_mul(n_u256)
                    .ok_or_else(|| MathError::Overflow {
                        operation: "calculate_d".to_string(),
                        inputs: vec![alloy_to_ethers(balance), alloy_to_ethers(n_u256)],
                        context: "balance * n (hoisted for D_P)".to_string(),
                    })?,
            );
        }
        &balance_times_n_vec[..]
    };

    const MAX_ITERATIONS: usize = 255;

    let mut d = sum_x;
    let mut prev_d;

    for _iteration in 0..MAX_ITERATIONS {
        let mut d_p = d;

        for &balance_times_n_i in balance_times_n {
            let divisor = match variant {
                StableswapMathVariant::Vyper02ThreePool => balance_times_n_i,
                StableswapMathVariant::Vyper01Legacy => balance_times_n_i.checked_add(ONE).ok_or_else(
                    || MathError::Overflow {
                        operation: "calculate_d".to_string(),
                        inputs: vec![alloy_to_ethers(balance_times_n_i)],
                        context: "x*N+1 for legacy D_P".to_string(),
                    },
                )?,
            };
            if divisor.is_zero() {
                return Err(MathError::DivisionByZero {
                    operation: "calculate_d".to_string(),
                    context: "D_P divisor is zero".to_string(),
                });
            }
            d_p = d_p
                .checked_mul(d)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calculate_d".to_string(),
                    inputs: vec![alloy_to_ethers(d_p), alloy_to_ethers(d)],
                    context: "d_p * d in D_P calculation".to_string(),
                })?
                .checked_div(divisor)
                .ok_or_else(|| MathError::DivisionByZero {
                    operation: "calculate_d".to_string(),
                    context: "D_P division".to_string(),
                })?;
        }

        prev_d = d;

        let ann_s = ann.checked_mul(sum_x).ok_or_else(|| MathError::Overflow {
            operation: "calculate_d".to_string(),
            inputs: vec![alloy_to_ethers(ann), alloy_to_ethers(sum_x)],
            context: "Ann * S".to_string(),
        })?;

        let d_p_n = d_p.checked_mul(n_u256).ok_or_else(|| MathError::Overflow {
            operation: "calculate_d".to_string(),
            inputs: vec![alloy_to_ethers(d_p), alloy_to_ethers(n_u256)],
            context: "D_P * N".to_string(),
        })?;

        let numerator_inner = ann_s
            .checked_add(d_p_n)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_d".to_string(),
                inputs: vec![alloy_to_ethers(ann_s), alloy_to_ethers(d_p_n)],
                context: "Ann * S + D_P * N".to_string(),
            })?;

        let numerator = numerator_inner
            .checked_mul(d)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_d".to_string(),
                inputs: vec![alloy_to_ethers(numerator_inner), alloy_to_ethers(d)],
                context: "(Ann * S + D_P * N) * D".to_string(),
            })?;

        let term1 = ann_minus_1
            .checked_mul(d)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_d".to_string(),
                inputs: vec![alloy_to_ethers(ann_minus_1), alloy_to_ethers(d)],
                context: "(Ann - 1) * D".to_string(),
            })?;

        let term2 = n_plus_1
            .checked_mul(d_p)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_d".to_string(),
                inputs: vec![alloy_to_ethers(n_plus_1), alloy_to_ethers(d_p)],
                context: "(N + 1) * D_P".to_string(),
            })?;

        let denominator = term1
            .checked_add(term2)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_d".to_string(),
                inputs: vec![alloy_to_ethers(term1), alloy_to_ethers(term2)],
                context: "(Ann - 1) * D + (N + 1) * D_P".to_string(),
            })?;

        if denominator == ZERO {
            return Err(MathError::DivisionByZero {
                operation: "calculate_d".to_string(),
                context: "Newton iteration denominator is zero".to_string(),
            });
        }

        d = numerator / denominator;

        let diff = if d > prev_d { d - prev_d } else { prev_d - d };
        if diff <= ONE {
            return Ok(d);
        }
    }

    return Err(MathError::InvalidInput {
        operation: "calculate_d".to_string(),
        reason: "Newton's method did not converge after 255 iterations".to_string(),
        context: format!("Last D: {}, n: {}", d, n),
    });
}

/// Calculate y given modified balances and the invariant D (Curve `get_y` logic).
///
/// Newton's method with iterative D_P; `_x` is ignored (balances already in `xp`).
///
/// # Arguments
/// * `i` - Index of input token (ignored in calculation, kept for API compatibility)
/// * `j` - Index of output token  
/// * `_x` - Input amount (ignored, xp should already contain the new balance)
/// * `xp` - Modified balances array (with swap already applied to input token)
/// * `a` - Amplification coefficient
/// * `d` - Current invariant value
///
/// # Returns
/// * `Ok(U256)` - The balance y for token j that maintains the invariant
/// * `Err(MathError)` - Calculation error
pub fn calculate_y(
    i: usize,
    j: usize,
    _x: U256,
    xp: &[U256],
    a: U256,
    d: U256,
) -> Result<U256, MathError> {
    if i == j {
        return Err(MathError::InvalidInput {
            operation: "calculate_y".to_string(),
            reason: "Input and output tokens cannot be the same".to_string(),
            context: format!("i={}, j={}", i, j),
        });
    }

    let n = xp.len();
    if j >= n {
        return Err(MathError::InvalidInput {
            operation: "calculate_y".to_string(),
            reason: "Output token index out of bounds".to_string(),
            context: format!("j={}, len={}", j, n),
        });
    }

    if n == 0 {
        return Err(MathError::InvalidInput {
            operation: "calculate_y".to_string(),
            reason: "Empty balances array".to_string(),
            context: "".to_string(),
        });
    }

    let n_u256 = U256::from(n as u64);

    let ann = a.checked_mul(n_u256).ok_or_else(|| MathError::Overflow {
        operation: "calculate_y".to_string(),
        inputs: vec![alloy_to_ethers(a), alloy_to_ethers(n_u256)],
        context: "Ann = A * N_COINS".to_string(),
    })?;

    let mut c = d;
    let mut s = ZERO;

    for (k, &xp_k) in xp.iter().enumerate() {
        if k != j {
            if xp_k == ZERO {
                return Err(MathError::DivisionByZero {
                    operation: "calculate_y".to_string(),
                    context: format!("Balance at index {} is zero", k),
                });
            }

            s = s.checked_add(xp_k).ok_or_else(|| MathError::Overflow {
                operation: "calculate_y".to_string(),
                inputs: vec![alloy_to_ethers(s), alloy_to_ethers(xp_k)],
                context: "Sum calculation".to_string(),
            })?;

            let xp_k_times_n = xp_k
                .checked_mul(n_u256)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calculate_y".to_string(),
                    inputs: vec![alloy_to_ethers(xp_k), alloy_to_ethers(n_u256)],
                    context: "xp_k * n".to_string(),
                })?;

            c = c
                .checked_mul(d)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calculate_y".to_string(),
                    inputs: vec![alloy_to_ethers(c), alloy_to_ethers(d)],
                    context: "c * D in iterative calculation".to_string(),
                })?
                .checked_div(xp_k_times_n)
                .ok_or_else(|| MathError::DivisionByZero {
                    operation: "calculate_y".to_string(),
                    context: "c / (xp_k * n)".to_string(),
                })?;
        }
    }

    c = c
        .checked_mul(d)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_y".to_string(),
            inputs: vec![alloy_to_ethers(c), alloy_to_ethers(d)],
            context: "Final c * D".to_string(),
        })?
        .checked_div(ann.checked_mul(n_u256).ok_or_else(|| MathError::Overflow {
            operation: "calculate_y".to_string(),
            inputs: vec![alloy_to_ethers(ann), alloy_to_ethers(n_u256)],
            context: "Ann * n".to_string(),
        })?)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_y".to_string(),
            context: "c / (Ann * n)".to_string(),
        })?;

    let d_over_ann = d
        .checked_div(ann)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_y".to_string(),
            context: "D / Ann".to_string(),
        })?;

    let b_intermediate = s
        .checked_add(d_over_ann)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_y".to_string(),
            inputs: vec![alloy_to_ethers(s), alloy_to_ethers(d_over_ann)],
            context: "S + D/Ann".to_string(),
        })?;

    let mut y = d;
    let mut prev_y;

    const MAX_ITERATIONS: usize = 255;

    for _iteration in 0..MAX_ITERATIONS {
        prev_y = y;

        let y_squared = y.checked_mul(y).ok_or_else(|| MathError::Overflow {
            operation: "calculate_y".to_string(),
            inputs: vec![alloy_to_ethers(y), alloy_to_ethers(y)],
            context: "y^2".to_string(),
        })?;

        let numerator = y_squared
            .checked_add(c)
            .ok_or_else(|| MathError::Overflow {
                operation: "calculate_y".to_string(),
                inputs: vec![alloy_to_ethers(y_squared), alloy_to_ethers(c)],
                context: "y^2 + c".to_string(),
            })?;

        let two_y = y << 1u32;

        let denominator_before_d =
            two_y
                .checked_add(b_intermediate)
                .ok_or_else(|| MathError::Overflow {
                    operation: "calculate_y".to_string(),
                    inputs: vec![alloy_to_ethers(two_y), alloy_to_ethers(b_intermediate)],
                    context: "2*y + b".to_string(),
                })?;

        let denominator = if denominator_before_d >= d {
            denominator_before_d - d
        } else {
            return Err(MathError::InvalidInput {
                operation: "calculate_y".to_string(),
                reason: "Newton denominator would be negative".to_string(),
                context: format!("2y+b={}, d={}", denominator_before_d, d),
            });
        };

        if denominator == ZERO {
            return Err(MathError::DivisionByZero {
                operation: "calculate_y".to_string(),
                context: "Newton iteration denominator is zero".to_string(),
            });
        }

        y = numerator / denominator;

        let diff = if y > prev_y { y - prev_y } else { prev_y - y };
        if diff <= ONE {
            return Ok(y);
        }
    }

    return Err(MathError::InvalidInput {
        operation: "calculate_y".to_string(),
        reason: "Newton's method did not converge after 255 iterations".to_string(),
        context: format!("Last y: {}, D: {}, n: {}", y, d, n),
    });
}

/// `get_dy` output: raw token `j` for `dx_raw` of token `i`.
///
/// `xp` and `rates` must match on-chain `_xp()` / `_stored_rates()` (see [`stableswap_rates_resolve`]).
/// Prefer `fee_raw` from `fee()` when non-zero so integer fee matches the contract when `fee_bps` decode is lossy.
pub fn calculate_dy(
    i: usize,
    j: usize,
    dx_raw: U256,
    xp: &[U256],
    rates: &[U256],
    variant: StableswapMathVariant,
    a: U256,
    fee_raw: U256,
    fee_bps: u32,
) -> Result<U256, MathError> {
    let n = xp.len();

    if rates.len() != n {
        return Err(MathError::InvalidInput {
            operation: "calculate_dy".to_string(),
            reason: format!("rates len {} != xp len {}", rates.len(), n),
            context: "".to_string(),
        });
    }

    if i >= n || j >= n {
        return Err(MathError::InvalidInput {
            operation: "calculate_dy".to_string(),
            reason: "Token index out of bounds".to_string(),
            context: format!("i={}, j={}, n={}", i, j, n),
        });
    }

    if i == j {
        return Err(MathError::InvalidInput {
            operation: "calculate_dy".to_string(),
            reason: "Cannot swap token with itself".to_string(),
            context: format!("i={}, j={}", i, j),
        });
    }

    let d = calculate_d(xp, a, n, variant)?;

    let rate_i = rates[i];
    let rate_j = rates[j];

    let dx_xp = dx_raw
        .checked_mul(rate_i)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_dy".to_string(),
            inputs: vec![alloy_to_ethers(dx_raw), alloy_to_ethers(rate_i)],
            context: "dx * rates[i]".to_string(),
        })?
        .checked_div(CURVE_PRECISION)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_dy".to_string(),
            context: "dx * rates[i] / PRECISION".to_string(),
        })?;

    let mut xp_modified: Vec<U256> = xp.to_vec();
    xp_modified[i] = xp_modified[i]
        .checked_add(dx_xp)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_dy".to_string(),
            inputs: vec![alloy_to_ethers(xp[i]), alloy_to_ethers(dx_xp)],
            context: "xp[i] + dx_xp".to_string(),
        })?;

    let y = calculate_y(i, j, dx_xp, &xp_modified, a, d)?;

    if y >= xp[j] {
        return Ok(ZERO);
    }

    let delta_xp = xp[j].checked_sub(y).ok_or_else(|| MathError::Underflow {
        operation: "calculate_dy".to_string(),
        inputs: vec![alloy_to_ethers(xp[j]), alloy_to_ethers(y)],
        context: "xp[j] - y".to_string(),
    })?;

    let numer = match variant {
        StableswapMathVariant::Vyper02ThreePool => {
            if delta_xp <= ONE {
                return Ok(ZERO);
            }
            delta_xp - ONE
        }
        StableswapMathVariant::Vyper01Legacy => {
            if delta_xp.is_zero() {
                return Ok(ZERO);
            }
            delta_xp
        }
    };

    let pre_fee_dy = numer
        .checked_mul(CURVE_PRECISION)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_dy".to_string(),
            inputs: vec![],
            context: "numer * PRECISION".to_string(),
        })?
        .checked_div(rate_j)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_dy".to_string(),
            context: "numer * PRECISION / rates[j]".to_string(),
        })?;

    let fee_scalar = curve_fee_scalar(fee_raw, fee_bps);
    let fee_amt = pre_fee_dy
        .checked_mul(fee_scalar)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_dy".to_string(),
            inputs: vec![alloy_to_ethers(pre_fee_dy), alloy_to_ethers(fee_scalar)],
            context: "fee * dy".to_string(),
        })?
        .checked_div(CURVE_FEE_DENOMINATOR)
        .ok_or_else(|| MathError::DivisionByZero {
            operation: "calculate_dy".to_string(),
            context: "fee * dy / FEE_DENOMINATOR".to_string(),
        })?;

    pre_fee_dy.checked_sub(fee_amt).ok_or_else(|| MathError::Underflow {
        operation: "calculate_dy".to_string(),
        inputs: vec![alloy_to_ethers(pre_fee_dy), alloy_to_ethers(fee_amt)],
        context: "dy after fee".to_string(),
    })
}

/// Calculate swap output for **StableSwap-N** pools via `calculate_dy`.
///
/// Despite the historical name, this is **not** Curve v2 crypto/tricrypto math — those pools must use
/// on-chain `get_dy` ([`crate::dex::curve::quoter`]) per `curve_pool_registry.json`.
///
/// # Arguments
/// * `amount_in` - Input token amount
/// * `token_in_index` - Index of input token in pool
/// * `token_out_index` - Index of output token in pool
/// * `balances` - Current pool balances
/// * `a` - Amplification coefficient
///
/// # Returns
/// * `Ok(U256)` - Output amount
/// * `Err(String)` - Calculation error
pub fn calculate_swap_output(
    amount_in: U256,
    token_in_index: usize,
    token_out_index: usize,
    balances: &[U256],
    decimals: &[u8],
    stored_rates: Option<&[U256]>,
    variant: StableswapMathVariant,
    a: U256,
    fee_raw: U256,
    fee_bps: u32,
) -> Result<U256, MathError> {
    let rates = stableswap_rates_resolve(decimals, stored_rates)?;
    let xp = stableswap_xp_from_rates(balances, &rates)?;
    calculate_dy(
        token_in_index,
        token_out_index,
        amount_in,
        &xp,
        &rates,
        variant,
        a,
        fee_raw,
        fee_bps,
    )
}

/// Calculate spot price for Curve cryptoswap
///
/// Price = dy/dx for infinitesimal amounts. This approximates the marginal price.
///
/// # Arguments
/// * `token_in_index` - Index of input token
/// * `token_out_index` - Index of output token
/// * `balances` - Current pool balances
/// * `a` - Amplification coefficient
///
/// # Returns
/// * `Ok(U256)` - Spot price with appropriate scaling
/// * `Err(String)` - Calculation error
pub fn calculate_curve_price(
    token_in_index: usize,
    token_out_index: usize,
    xp: &[U256],
    rates: &[U256],
    variant: StableswapMathVariant,
    a: U256,
) -> Result<U256, MathError> {
    let dy = calculate_dy(
        token_in_index,
        token_out_index,
        TEST_AMOUNT,
        xp,
        rates,
        variant,
        a,
        ZERO,
        0,
    )?;

    let price = dy * WAD / TEST_AMOUNT;

    Ok(price)
}

// Helper functions for U256 arithmetic

/// Calculate power for U256 with overflow protection
/// Returns error if overflow would occur instead of silently returning MAX
fn pow_u256(base: U256, exp: usize) -> Result<U256, MathError> {
    if exp == 0 {
        return Ok(ONE);
    }
    if exp == 1 {
        return Ok(base);
    }

    let mut result = ONE;
    let mut base = base;
    let mut exp = exp;

    if base > ONE {
        let bits_per_mult = if base >= U256::from(1u128 << 64) {
            64
        } else if base >= U256::from(1u128 << 32) {
            32
        } else if base >= U256::from(1u128 << 16) {
            16
        } else if base >= U256::from(256) {
            8
        } else {
            1
        };

        if bits_per_mult * exp > 256 {
            return Err(MathError::Overflow {
                operation: "pow_u256".to_string(),
                inputs: vec![alloy_to_ethers(base), alloy_to_ethers(U256::from(exp as u64))],
                context: format!("Exponent {} with base {} would overflow U256", exp, base),
            });
        }
    }

    while exp > 0 {
        if exp % 2 == 1 {
            if result != ZERO {
                if let Some(max_base) = U256::MAX.checked_div(result) {
                    if base > max_base {
                        return Err(MathError::Overflow {
                            operation: "pow_u256".to_string(),
                            inputs: vec![alloy_to_ethers(base), alloy_to_ethers(U256::from(exp as u64))],
                            context: format!(
                                "Multiplication overflow: result * base would exceed U256::MAX"
                            ),
                        });
                    }
                } else {
                    return Err(MathError::Overflow {
                        operation: "pow_u256".to_string(),
                        inputs: vec![alloy_to_ethers(base), alloy_to_ethers(U256::from(exp as u64))],
                        context: "Division by zero in overflow check".to_string(),
                    });
                }
            }
            result = result
                .checked_mul(base)
                .ok_or_else(|| MathError::Overflow {
                    operation: "pow_u256".to_string(),
                    inputs: vec![alloy_to_ethers(result), alloy_to_ethers(base)],
                    context: format!("Multiplication overflow in pow_u256"),
                })?;
        }

        if exp > 1 {
            if let Some(max_base) = U256::MAX.checked_div(base) {
                if base > max_base {
                    return Err(MathError::Overflow {
                        operation: "pow_u256".to_string(),
                        inputs: vec![alloy_to_ethers(base), alloy_to_ethers(U256::from(exp as u64))],
                        context: format!("Squaring overflow: base * base would exceed U256::MAX"),
                    });
                }
            }
            base = base.checked_mul(base).ok_or_else(|| MathError::Overflow {
                operation: "pow_u256".to_string(),
                inputs: vec![alloy_to_ethers(base), alloy_to_ethers(base)],
                context: "Squaring overflow in pow_u256".to_string(),
            })?;
        }

        exp /= 2;
    }

    Ok(result)
}

/// Calculate square root for U256 using Newton's method with high precision
///
/// This is a general-purpose integer square root used by Curve math
/// and can be reused by other DEX math modules (e.g., V3 price calculations)
/// Integer square root using Newton's method (Babylonian method)
///
/// Uses bit-width estimation for initial guess, achieving much faster convergence
/// than a naive (x+1)/2 starting point.
///
/// # Arguments
/// * `x` - Value to compute square root of
///
/// # Returns
/// * `Ok(U256)` - floor(sqrt(x))
/// * `Err(MathError)` - If calculation fails
pub fn sqrt_u256(x: U256) -> Result<U256, MathError> {
    if x <= ONE {
        return Ok(x);
    }

    let bits = 256 - x.leading_zeros();
    let mut z = ONE << ((bits + 1) / 2);

    loop {
        let x_div_z = x / z;
        let next = (z + x_div_z) >> 1;
        if next >= z {
            break;
        }
        z = next;
    }

    Ok(z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_d_simple() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let n = 2;

        let result = calculate_d(&balances, a, n, StableswapMathVariant::Vyper02ThreePool);
        assert!(result.is_ok(), "D calculation should succeed");

        let d = result.unwrap();
        assert!(d > ZERO, "Invariant D should be positive");
        assert!(
            d >= U256::from(1900000000000000000000u128),
            "D should be close to 2 * balance"
        );
        assert!(
            d <= U256::from(2100000000000000000000u128),
            "D should be close to 2 * balance"
        );
    }

    #[test]
    fn test_calculate_d_3_token() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let n = 3;

        let result = calculate_d(&balances, a, n, StableswapMathVariant::Vyper02ThreePool);
        assert!(
            result.is_ok(),
            "D calculation should succeed for 3-token pool"
        );

        let d = result.unwrap();
        assert!(d > ZERO, "Invariant D should be positive");
        assert!(
            d >= U256::from(2800000000000000000000u128),
            "D should be close to 3 * balance"
        );
        assert!(
            d <= U256::from(3200000000000000000000u128),
            "D should be close to 3 * balance"
        );
    }

    #[test]
    fn test_calculate_dy() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let dec = vec![18u8, 18u8];
        let rates = stableswap_rates_resolve(&dec, None).expect("rates");
        let xp = stableswap_xp_from_balances(&balances, &dec).expect("xp");
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);

        let result = calculate_dy(
            0,
            1,
            dx,
            &xp,
            &rates,
            StableswapMathVariant::Vyper02ThreePool,
            a,
            U256::from(4_000_000u64),
            4,
        );
        assert!(result.is_ok(), "Swap calculation should succeed");

        let dy = result.unwrap();
        assert!(dy > ZERO, "Should receive some output tokens");
        assert!(
            dy < dx,
            "Output should be less than input due to fees/slippage"
        );
    }

    #[test]
    fn test_zero_balance() {
        let balances = vec![ZERO, U256::from(1000)];
        let a = U256::from(100);
        let result = calculate_d(&balances, a, 2, StableswapMathVariant::Vyper02ThreePool);
        assert_eq!(
            result.unwrap(),
            ZERO,
            "Zero balance should result in D = 0"
        );
    }

    #[test]
    fn test_calculate_y_2_token_simple() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);

        let d = calculate_d(
            &balances,
            a,
            2,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();

        let mut xp_modified = balances.clone();
        xp_modified[0] = xp_modified[0] + dx;

        let result = calculate_y(0, 1, dx, &xp_modified, a, d);
        assert!(
            result.is_ok(),
            "calculate_y should succeed for 2-token pool"
        );

        let y = result.unwrap();
        assert!(y > ZERO, "y should be positive");
        assert!(y < balances[1], "y should be less than balance");
    }

    #[test]
    fn test_calculate_y_3_token() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);

        let d = calculate_d(
            &balances,
            a,
            3,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();

        let mut xp_modified = balances.clone();
        xp_modified[0] = xp_modified[0] + dx;

        let result = calculate_y(0, 1, dx, &xp_modified, a, d);
        assert!(
            result.is_ok(),
            "calculate_y should succeed for 3-token pool"
        );

        let y = result.unwrap();
        assert!(y > ZERO, "y should be positive");
        assert!(y < balances[1], "y should be less than balance");
    }

    #[test]
    fn test_calculate_y_small_amount() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let dx = U256::from(1000000000000u64);

        let d = calculate_d(
            &balances,
            a,
            2,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();
        let mut xp_modified = balances.clone();
        xp_modified[0] = xp_modified[0] + dx;

        let result = calculate_y(0, 1, dx, &xp_modified, a, d);
        assert!(
            result.is_ok(),
            "calculate_y should succeed for small amounts"
        );

        let y = result.unwrap();
        assert!(
            y > ZERO,
            "y should be positive even for small amounts"
        );
    }

    #[test]
    fn test_calculate_y_large_amount() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let dx = U256::from(100_000_000_000_000_000_000u128);

        let d = calculate_d(
            &balances,
            a,
            2,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();
        let mut xp_modified = balances.clone();
        xp_modified[0] = xp_modified[0] + dx;

        let result = calculate_y(0, 1, dx, &xp_modified, a, d);
        assert!(
            result.is_ok(),
            "calculate_y should succeed for large amounts"
        );

        let y = result.unwrap();
        assert!(y > ZERO, "y should be positive");
        assert!(
            y < balances[1],
            "y should be less than original balance (swap effect)"
        );
    }

    #[test]
    fn test_calculate_y_consistency_with_calculate_dy() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let dec = vec![18u8, 18u8];
        let rates = stableswap_rates_resolve(&dec, None).expect("rates");
        let xp = stableswap_xp_from_balances(&balances, &dec).expect("xp");
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);
        let fee_bps = 4u32;

        let d = calculate_d(
            &xp,
            a,
            2,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();
        let rate_j = stableswap_rate_for_decimals(18).unwrap();
        let dx_xp = dx
            .checked_mul(stableswap_rate_for_decimals(18).unwrap())
            .unwrap()
            .checked_div(CURVE_PRECISION)
            .unwrap();
        let mut xp_modified = xp.clone();
        xp_modified[0] = xp_modified[0] + dx_xp;
        let y = calculate_y(0, 1, dx_xp, &xp_modified, a, d).expect("calculate_y");

        let delta_xp = xp[1].checked_sub(y).expect("xp[1]>=y");
        assert!(delta_xp > ONE);
        let pre_fee_dy = (delta_xp - ONE)
            .checked_mul(CURVE_PRECISION)
            .unwrap()
            .checked_div(rate_j)
            .unwrap();
        let fee_amt = pre_fee_dy
            .checked_mul(curve_fee_raw_from_bps(fee_bps))
            .unwrap()
            .checked_div(CURVE_FEE_DENOMINATOR)
            .unwrap();
        let dy_from_y = pre_fee_dy.checked_sub(fee_amt).unwrap();

        let dy_calc = calculate_dy(
            0,
            1,
            dx,
            &xp,
            &rates,
            StableswapMathVariant::Vyper02ThreePool,
            a,
            U256::from(4_000_000u64),
            fee_bps,
        )
        .expect("calculate_dy");
        assert_eq!(dy_calc, dy_from_y, "get_y + Vyper rounding should match calculate_dy");
    }

    #[test]
    fn test_pow_u256_overflow() {
        let large_base = WAD;
        let result = pow_u256(large_base, 10);
        assert!(result.is_err(), "Large power should return overflow error");

        if let Err(MathError::Overflow { .. }) = result {
            // Correct error type
        } else {
            panic!("Expected Overflow error");
        }
    }

    #[test]
    fn test_pow_u256_normal() {
        assert_eq!(pow_u256(TWO, 8).unwrap(), U256::from(256));
        assert_eq!(pow_u256(U256::from(10), 2).unwrap(), U256::from(100));
        assert_eq!(pow_u256(U256::from(5), 0).unwrap(), ONE);
        assert_eq!(pow_u256(U256::from(7), 1).unwrap(), U256::from(7));
    }

    #[test]
    fn test_pow_u256_edge_cases() {
        assert_eq!(pow_u256(ONE, 100).unwrap(), ONE);
        assert_eq!(pow_u256(TWO, 0).unwrap(), ONE);
        assert_eq!(pow_u256(TWO, 1).unwrap(), TWO);

        let result = pow_u256(TWO, 255);
        assert!(result.is_ok(), "2^255 should succeed");
    }

    #[test]
    fn test_calculate_y_with_overflow_protection() {
        let large_balance = U256::from(10).pow(U256::from(30));
        let balances = vec![large_balance, large_balance];
        let dec = vec![18u8, 18u8];
        let rates = stableswap_rates_resolve(&dec, None).expect("rates");
        let xp = stableswap_xp_from_balances(&balances, &dec).expect("xp");
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);

        let result = calculate_dy(
            0,
            1,
            dx,
            &xp,
            &rates,
            StableswapMathVariant::Vyper02ThreePool,
            a,
            U256::from(4_000_000u64),
            4,
        );
        assert!(
            result.is_ok() || matches!(result, Err(MathError::Overflow { .. })),
            "calculate_dy should handle overflow gracefully"
        );
    }

    #[test]
    fn test_sqrt_u256_precision() {
        let perfect_square = U256::from(1000000000000000000u128);
        let result = sqrt_u256(perfect_square);
        assert!(result.is_ok(), "sqrt should succeed for perfect square");
        let sqrt_val = result.unwrap();

        let expected = U256::from(1000000000u128);
        let diff = if sqrt_val > expected {
            sqrt_val - expected
        } else {
            expected - sqrt_val
        };

        assert!(
            diff < expected / BPS_DENOM,
            "sqrt precision should be within 0.01%"
        );
    }

    #[test]
    fn test_sqrt_u256_convergence() {
        let large_number = U256::from(10).pow(U256::from(36));
        let result = sqrt_u256(large_number);
        assert!(result.is_ok(), "sqrt should converge for large numbers");

        let sqrt_val = result.unwrap();
        let expected = WAD;
        assert!(
            sqrt_val > expected / TWO,
            "sqrt should be reasonable"
        );
        assert!(
            sqrt_val < expected * TWO,
            "sqrt should be reasonable"
        );
    }

    #[test]
    fn test_sqrt_u256_edge_cases() {
        assert_eq!(sqrt_u256(ZERO).unwrap(), ZERO);
        assert_eq!(sqrt_u256(ONE).unwrap(), ONE);
        assert_eq!(sqrt_u256(FOUR).unwrap(), TWO);
        assert_eq!(sqrt_u256(U256::from(9)).unwrap(), THREE);

        let very_large = U256::MAX / TWO;
        let result = sqrt_u256(very_large);
        assert!(result.is_ok(), "sqrt should handle very large numbers");
    }

    #[test]
    fn test_sqrt_u256_used_in_calculate_y() {
        let balances = vec![
            U256::from(1000000000000000000000u128),
            U256::from(1000000000000000000000u128),
        ];
        let a = U256::from(100);
        let dx = U256::from(1000000000000000000u64);

        let d = calculate_d(
            &balances,
            a,
            2,
            StableswapMathVariant::Vyper02ThreePool,
        )
        .unwrap();
        let mut xp_modified = balances.clone();
        xp_modified[0] = xp_modified[0] + dx;

        let result = calculate_y(0, 1, dx, &xp_modified, a, d);
        assert!(
            result.is_ok(),
            "calculate_y should work with improved sqrt precision"
        );
    }

    #[test]
    fn test_calculate_d_with_overflow_protection() {
        let large_balance = U256::from(10).pow(U256::from(30));
        let balances = vec![large_balance, large_balance, large_balance];
        let a = U256::from(100);

        let result = calculate_d(
            &balances,
            a,
            3,
            StableswapMathVariant::Vyper02ThreePool,
        );
        assert!(
            result.is_ok() || matches!(result, Err(MathError::Overflow { .. })),
            "calculate_d should handle overflow gracefully"
        );
    }

    // #[test]
    // fn test_same_token_indices() {
    //     let balances = vec![U256::from(1000), U256::from(1000)];
    //     let a = U256::from(100);
    //     let result = calculate_dy(0, 0, U256::from(100), &balances, a);
    // assert!(result.is_err(), "Same token indices should return error");
    // }
}

pub fn calculate_curve_post_frontrun_balances(
    frontrun_amount: U256,
    balances: &[U256],
    decimals: &[u8],
    stored_rates: Option<&[U256]>,
    variant: StableswapMathVariant,
    amplification: U256,
    fee_raw: U256,
    fee_bps: u32,
) -> Result<Vec<U256>, MathError> {
    let frontrun_token_in = 0;
    let frontrun_token_out = 1;

    let rates = stableswap_rates_resolve(decimals, stored_rates)?;
    let xp = stableswap_xp_from_rates(balances, &rates)?;
    let frontrun_output = calculate_dy(
        frontrun_token_in,
        frontrun_token_out,
        frontrun_amount,
        &xp,
        &rates,
        variant,
        amplification,
        fee_raw,
        fee_bps,
    )?;
    let mut new_balances = balances.to_vec();
    new_balances[frontrun_token_in] = new_balances[frontrun_token_in]
        .checked_add(frontrun_amount)
        .ok_or_else(|| MathError::Overflow {
            operation: "calculate_curve_post_frontrun_balances".to_string(),
            inputs: vec![alloy_to_ethers(balances[frontrun_token_in]), alloy_to_ethers(frontrun_amount)],
            context: "Balance in".to_string(),
        })?;
    new_balances[frontrun_token_out] = new_balances[frontrun_token_out]
        .checked_sub(frontrun_output)
        .ok_or_else(|| MathError::Underflow {
            operation: "calculate_curve_post_frontrun_balances".to_string(),
            inputs: vec![alloy_to_ethers(balances[frontrun_token_out]), alloy_to_ethers(frontrun_output)],
            context: "Balance out".to_string(),
        })?;
    Ok(new_balances)
}

pub fn calculate_curve_post_victim_balances(
    victim_amount: U256,
    balances: &[U256],
    decimals: &[u8],
    stored_rates: Option<&[U256]>,
    variant: StableswapMathVariant,
    amplification: U256,
    fee_raw: U256,
    fee_bps: u32,
) -> Result<Vec<U256>, MathError> {
    calculate_curve_post_frontrun_balances(
        victim_amount,
        balances,
        decimals,
        stored_rates,
        variant,
        amplification,
        fee_raw,
        fee_bps,
    )
}
