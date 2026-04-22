//! Type conversion utilities for Balancer math crate integration
//!
//! Provides zero-copy conversions between primitive_types::U256 and
//! alloy_primitives::U256 for use with balancer-maths-rust crate.
//! Both types use the same internal representation (4 u64 limbs),
//! allowing efficient conversion.

use primitive_types::U256 as PrimitiveU256;
use alloy_primitives::U256 as AlloyU256;

/// 10^14 for bps → 18-decimal fee scaling (compile-time constant; avoids runtime `pow`).
const TEN_POW_14: PrimitiveU256 = PrimitiveU256([100_000_000_000_000u64, 0, 0, 0]);

/// Convert primitive_types::U256 to alloy_primitives::U256
/// Zero-copy conversion using internal limb representation
#[inline(always)]
pub fn to_alloy_u256(val: PrimitiveU256) -> AlloyU256 {
    let limbs = val.0;
    AlloyU256::from_limbs([limbs[0], limbs[1], limbs[2], limbs[3]])
}

/// Convert alloy_primitives::U256 to primitive_types::U256
/// Zero-cost conversion via direct limb copy — both types use [u64; 4] little-endian
#[inline(always)]
pub fn to_primitive_u256(val: AlloyU256) -> PrimitiveU256 {
    PrimitiveU256(val.into_limbs())
}

/// Convert swap fee from basis points (u32) to 18-decimal format
/// Formula: bps * 10^14 = 18-decimal representation
/// Example: 30 bps (0.3%) = 30 * 10^14 = 3 * 10^15
#[inline(always)]
pub fn swap_fee_bps_to_18_decimal(bps: u32) -> PrimitiveU256 {
    PrimitiveU256::from(bps) * TEN_POW_14
}

use crate::core::MathError;
use balancer_maths_rust::common::errors::PoolError;

/// Map balancer-maths-rust PoolError to our MathError
/// Ensures consistent error handling across the codebase
pub fn map_pool_error_to_math_error(err: PoolError, operation: &str) -> MathError {
    match err {
        PoolError::InvalidInput(reason) => MathError::InvalidInput {
            operation: operation.to_string(),
            reason,
            context: "Balancer math crate error".to_string(),
        },
        _ => MathError::InvalidInput {
            operation: operation.to_string(),
            reason: format!("Balancer math crate error: {:?}", err),
            context: "Unknown crate error".to_string(),
        },
    }
}

