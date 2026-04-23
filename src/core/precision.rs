//! Precision-safe types - 100% accuracy using U256
//! NO floating point in financial calculations!


use crate::core::error::MathError;
use ethers_core::types::U256;

/// Basis points (0-10000 where 10000 = 100%)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BasisPoints(pub u32);

impl BasisPoints {
    pub const ZERO: Self = Self(0);
    pub const ONE_PERCENT: Self = Self(100);
    pub const MAX: Self = Self(10000);

    pub fn new(bps: u32) -> Result<Self, MathError> {
        if bps > 10000 {
            return Err(MathError::InvalidInput {
                operation: "BasisPoints::new".to_string(),
                reason: format!("{} > 10000", bps),
                context: "".to_string(),
            });
        }
        Ok(Self(bps))
    }

    pub const fn new_const(bps: u32) -> Self {
        debug_assert!(bps <= 10000, "BasisPoints::new_const out of range");
        Self(bps)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

/// Multiply by basis points: amount * bps / 10000
pub fn mul_basis_points(amount: U256, bps: BasisPoints) -> Result<U256, MathError> {
    let bps_u256 = U256::from(bps.as_u32());
    let scale = U256::from(10000u32);

    amount
        .checked_mul(bps_u256)
        .ok_or_else(|| MathError::Overflow {
            operation: "mul_basis_points".to_string(),
            inputs: vec![amount, bps_u256],
            context: "".to_string(),
        })
        .map(|n| n / scale)
}

/// Divide by basis points: amount * 10000 / bps
pub fn div_basis_points(amount: U256, bps: BasisPoints) -> Result<U256, MathError> {
    if bps.as_u32() == 0 {
        return Err(MathError::DivisionByZero {
            operation: "div_basis_points".to_string(),
            context: "".to_string(),
        });
    }

    let scale = U256::from(10000u32);
    amount
        .checked_mul(scale)
        .ok_or_else(|| MathError::Overflow {
            operation: "div_basis_points".to_string(),
            inputs: vec![amount, scale],
            context: "".to_string(),
        })
        .map(|n| n / U256::from(bps.as_u32()))
}

/// Convert U256 to f64 for DISPLAY ONLY (never for calculations!)
pub fn to_f64_for_display(amount: U256) -> f64 {
    let divisor = U256::from(10u128.pow(18));
    let integer = amount / divisor;
    let fraction = amount % divisor;
    integer.as_u128() as f64 + (fraction.as_u128() as f64 / 1e18)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactPrice(pub U256);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExactAmount(pub U256);

impl ExactAmount {
    pub fn new(amount: U256) -> Self {
        Self(amount)
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExactProfit(pub U256);

impl ExactProfit {
    pub fn is_profitable(&self) -> bool {
        self.0 > U256::zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basis_points() {
        assert!(BasisPoints::new(100).is_ok());
        assert!(BasisPoints::new(10001).is_err());
    }

    #[test]
    fn test_mul_basis_points() {
        let amount = U256::from(1000u64);
        let bps = BasisPoints::ONE_PERCENT;
        let result = mul_basis_points(amount, bps).unwrap();
        assert_eq!(result, U256::from(10u64));
    }
}
