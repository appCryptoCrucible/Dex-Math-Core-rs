//! DEX Adapter trait - unified interface for all DEX types
//!
//! Provides consistent methods for swap calculations, price impact, and optimal sizing
//! across all supported DEX implementations.


use crate::core::error::{DexError, MathError};
use crate::core::types::{DexType, PoolKey};
use crate::data::{PoolState, PoolStateProvider};
use ethers_core::types::U256;

/// Swap direction for calculations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapDirection {
    Token0ToToken1,
    Token1ToToken0,
}

/// Swap calculation result
#[derive(Debug, Clone)]
pub struct SwapResult {
    pub amount_out: U256,
    pub price_impact_bps: u32,
    pub gas_estimate: U256,
    pub execution_price: U256,
}

/// Unified trait for all DEX adapters
pub trait DexAdapter {
    /// Get the DEX type this adapter handles
    fn dex_type(&self) -> DexType;

    /// Get self as Any for downcasting
    fn as_any(&self) -> &dyn std::any::Any;

    /// Calculate swap output for given input
    ///
    /// # Arguments
    /// * `pool_key` - Pool identifier
    /// * `amount_in` - Input token amount
    /// * `direction` - Swap direction
    /// * `pool_manager` - Pool state manager
    /// * `price_cache` - Price cache for validation
    ///
    /// # Returns
    /// * `Ok(SwapResult)` - Swap calculation result
    /// * `Err(DexError)` - If calculation fails
    fn calculate_swap(
        &self,
        pool_key: &PoolKey,
        amount_in: U256,
        direction: SwapDirection,
        pool_state_provider: &dyn PoolStateProvider,
    ) -> Result<SwapResult, DexError>;

    /// Calculate swap output with optional post-execution state
    ///
    /// # Arguments
    /// * `pool_key` - Pool identifier
    /// * `amount_in` - Input token amount
    /// * `direction` - Swap direction
    /// * `pool_manager` - Pool state manager
    /// * `price_cache` - Price cache for validation
    /// * `post_state` - Optional post-execution pool state for simulation
    ///
    /// # Returns
    /// * `Ok(SwapResult)` - Swap calculation result
    /// * `Err(DexError)` - If calculation fails
    fn calculate_swap_with_state(
        &self,
        pool_key: &PoolKey,
        amount_in: U256,
        direction: SwapDirection,
        pool_state_provider: &dyn PoolStateProvider,
        _post_state: Option<&PoolState>,
    ) -> Result<SwapResult, DexError> {
        // Default implementation calls calculate_swap
        self.calculate_swap(pool_key, amount_in, direction, pool_state_provider)
    }

    /// Get current pool price
    ///
    /// # Arguments
    /// * `pool_key` - Pool identifier
    /// * `pool_manager` - Pool state manager
    ///
    /// # Returns
    /// * `Ok(U256)` - Current price (token1 per token0)
    /// * `Err(DexError)` - If price calculation fails
    fn get_current_price(
        &self,
        pool_key: &PoolKey,
        pool_state_provider: &dyn PoolStateProvider,
    ) -> Result<U256, DexError>;

    /// Validate pool state is fresh enough for calculations
    ///
    /// # Arguments
    /// * `pool_key` - Pool identifier
    /// * `pool_manager` - Pool state manager
    /// * `max_age_blocks` - Maximum age in blocks
    ///
    /// # Returns
    /// * `Ok(())` - Pool state is fresh
    /// * `Err(DexError)` - Pool state is stale or missing
    fn validate_pool_freshness(
        &self,
        pool_key: &PoolKey,
        pool_state_provider: &dyn PoolStateProvider,
        max_age_blocks: u64,
    ) -> Result<(), DexError>;
}

/// Helper functions for common calculations
pub mod helpers {
    use super::*;
    use crate::dex::common::{alloy_to_ethers, ethers_to_alloy};
    use uniswap_v3_math::full_math;

    const WAD: U256 = U256([1_000_000_000_000_000_000u64, 0, 0, 0]);

    /// Calculate execution price from swap result
    pub fn calculate_execution_price(
        amount_in: U256,
        amount_out: U256,
        direction: SwapDirection,
    ) -> Result<U256, MathError> {
        if amount_in.is_zero() {
            return Err(MathError::DivisionByZero {
                operation: "calculate_execution_price".to_string(),
                context: "DEX adapter".to_string(),
            });
        }

        match direction {
            SwapDirection::Token0ToToken1 => {
                let numerator = ethers_to_alloy(amount_out);
                let denominator = ethers_to_alloy(amount_in);
                let execution_price = full_math::mul_div(numerator, ethers_to_alloy(WAD), denominator)
                    .map_err(|e| MathError::Overflow {
                        operation: "calculate_execution_price".to_string(),
                        inputs: vec![amount_out, WAD, amount_in],
                        context: format!("Token0ToToken1 mul_div failed: {}", e),
                    })?;
                Ok(alloy_to_ethers(execution_price))
            }
            SwapDirection::Token1ToToken0 => {
                if amount_out.is_zero() {
                    return Err(MathError::DivisionByZero {
                        operation: "calculate_execution_price".to_string(),
                        context: "amount_out is zero for Token1ToToken0".to_string(),
                    });
                }
                let numerator = ethers_to_alloy(amount_in);
                let denominator = ethers_to_alloy(amount_out);
                let execution_price = full_math::mul_div(numerator, ethers_to_alloy(WAD), denominator)
                    .map_err(|e| MathError::Overflow {
                        operation: "calculate_execution_price".to_string(),
                        inputs: vec![amount_in, WAD, amount_out],
                        context: format!("Token1ToToken0 mul_div failed: {}", e),
                    })?;
                Ok(alloy_to_ethers(execution_price))
            }
        }
    }

    /// Convert basis points to percentage
    pub fn bps_to_percentage(bps: u32) -> f64 {
        bps as f64 / 10000.0
    }

    /// Convert percentage to basis points
    pub fn percentage_to_bps(percentage: f64) -> Result<u32, MathError> {
        if percentage < 0.0 || percentage > 100.0 {
            return Err(MathError::InvalidInput {
                operation: "percentage_to_bps".to_string(),
                reason: format!("Percentage must be between 0 and 100, got: {}", percentage),
                context: "DEX adapter".to_string(),
            });
        }

        let bps = (percentage * 10000.0) as u32;
        Ok(bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_price_calculation() {
        let amount_in = U256::from(1000);
        let amount_out = U256::from(2000);

        let price = helpers::calculate_execution_price(
            amount_in,
            amount_out,
            SwapDirection::Token0ToToken1,
        )
        .unwrap();

        // Should be 2.0 * 10^18 (2 token1 per token0)
        assert_eq!(price, U256::from(2) * U256::from(10).pow(U256::from(18)));
    }

    #[test]
    fn test_bps_conversion() {
        assert_eq!(helpers::bps_to_percentage(100), 0.01);
        assert_eq!(helpers::bps_to_percentage(10000), 1.0);

        assert_eq!(helpers::percentage_to_bps(0.01).unwrap(), 100);
        assert_eq!(helpers::percentage_to_bps(1.0).unwrap(), 10000);
    }

    #[test]
    fn test_invalid_percentage() {
        let result = helpers::percentage_to_bps(-1.0);
        assert!(result.is_err());

        let result = helpers::percentage_to_bps(101.0);
        assert!(result.is_err());
    }
}
