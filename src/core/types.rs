//! Core domain types for DEX identification and pool management
//!
//! This module contains ONLY domain types (DexType, PoolKey).
//! Error types are in src/core/error.rs
//! Precision types (BasisPoints) are in src/core/precision.rs

use ethers_core::types::Address;

/// DEX types supported by the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    PancakeSwap,
    Kyber,
    Curve,
    Balancer,
    ShibaSwap,
}

/// Pool key for identifying specific pools across all DEX types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolKey {
    pub dex_type: DexType,
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
}

impl PoolKey {
    /// Create a new pool key
    pub fn new(dex_type: DexType, pool_address: Address, token0: Address, token1: Address) -> Self {
        Self {
            dex_type,
            pool_address,
            token0,
            token1,
        }
    }
}
