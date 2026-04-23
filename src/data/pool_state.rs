//! Pool state structures for all DEX types
//!
//! Each DEX type has its own state structure optimized for its specific AMM model.


use crate::core::types::DexType;
use crate::data::curve_registry::{
    CurveExecutionKind, CurveFamily, CurveQuotingStrategy, CurveRegistryEntry,
    StableswapMathVariant,
};
use crate::data::kyber_pool_state::KyberPoolState;
use ethers_core::types::{Address, U256};
use std::collections::HashMap;

/// V2 pool state (Uniswap V2, SushiSwap, PancakeSwap, ShibaSwap)
#[repr(align(64))] // Cache-line aligned
#[derive(Debug, Clone)]
pub struct V2PoolState {
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub last_update_block: u64,
    /// DEX this pool belongs to (for correct router/encoding)
    pub dex_type: DexType,
}

impl V2PoolState {
    pub fn new(pool_address: Address, token0: Address, token1: Address) -> Self {
        Self {
            pool_address,
            token0,
            token1,
            reserve0: U256::zero(),
            reserve1: U256::zero(),
            last_update_block: 0,
            dex_type: DexType::UniswapV2,
        }
    }

    pub fn with_dex_type(mut self, dex_type: DexType) -> Self {
        self.dex_type = dex_type;
        self
    }

    pub fn update_reserves(&mut self, reserve0: U256, reserve1: U256, block_number: u64) {
        self.reserve0 = reserve0;
        self.reserve1 = reserve1;
        self.last_update_block = block_number;
    }
}

/// V3 pool state (Uniswap V3, KyberElastic)
#[repr(align(64))] // Cache-line aligned
#[derive(Debug, Clone)]
pub struct V3PoolState {
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub liquidity: u128,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub fee_tier: u32,     // in basis points
    pub tick_spacing: i32, // CRITICAL: Required for accurate tick alignment
    pub last_update_block: u64,
    // Uniswap V3 ticks(tick).liquidityNet — signed; applied on tick crossing in swap math
    pub tick_liquidity_map: HashMap<i32, i128>,
    pub initialized_ticks: Vec<i32>,            // Sorted list of initialized tick boundaries
}

impl V3PoolState {
    pub fn new(pool_address: Address, token0: Address, token1: Address, fee_tier: u32) -> Self {
        // Calculate tick spacing from fee tier
        // V3 tick spacing: 0.01% = 1, 0.05% = 10, 0.3% = 60, 1% = 200
        let tick_spacing = match fee_tier {
            100 => 1,     // 0.01%
            500 => 10,    // 0.05%
            3000 => 60,   // 0.3%
            10000 => 200, // 1%
            _ => {
                tracing::warn!(fee_tier = fee_tier, pool = ?pool_address, "Unknown V3 fee tier, using tick_spacing=60");
                60
            }
        };

        Self {
            pool_address,
            token0,
            token1,
            liquidity: 0,
            sqrt_price_x96: U256::zero(),
            tick: 0,
            fee_tier,
            tick_spacing,
            last_update_block: 0,
            tick_liquidity_map: HashMap::new(),
            initialized_ticks: Vec::new(),
        }
    }

    pub fn update_state(
        &mut self,
        liquidity: u128,
        sqrt_price_x96: U256,
        tick: i32,
        block_number: u64,
    ) {
        self.liquidity = liquidity;
        self.sqrt_price_x96 = sqrt_price_x96;
        self.tick = tick;
        self.last_update_block = block_number;
    }

    /// Apply mint event (add liquidity to tick range)
    pub fn apply_mint(&mut self, tick_lower: i32, tick_upper: i32, amount: u128) {
        if amount == 0 || tick_lower >= tick_upper {
            return;
        }

        // Match Uniswap V3: lower tick += L, upper tick -= L
        let d = i128::try_from(amount).unwrap_or(i128::MAX);
        *self.tick_liquidity_map.entry(tick_lower).or_insert(0) += d;
        *self.tick_liquidity_map.entry(tick_upper).or_insert(0) -= d;

        // Update initialized ticks list (keep sorted)
        if !self.initialized_ticks.contains(&tick_lower) {
            self.initialized_ticks.push(tick_lower);
            self.initialized_ticks.sort();
        }
        if !self.initialized_ticks.contains(&tick_upper) {
            self.initialized_ticks.push(tick_upper);
            self.initialized_ticks.sort();
        }
    }

    /// Apply burn event (remove liquidity from tick range)
    pub fn apply_burn(&mut self, tick_lower: i32, tick_upper: i32, amount: u128) {
        if amount == 0 || tick_lower >= tick_upper {
            return;
        }

        let d = i128::try_from(amount).unwrap_or(i128::MAX);
        if let Some(lower_liq) = self.tick_liquidity_map.get_mut(&tick_lower) {
            *lower_liq -= d;
        }
        if let Some(upper_liq) = self.tick_liquidity_map.get_mut(&tick_upper) {
            *upper_liq += d;
        }
    }
}

/// V4 pool state (Uniswap V4 concentrated liquidity).
///
/// This intentionally stores only deterministic quote-critical fields.
/// Hook execution is not simulated here; unsupported hook/dynamic-fee
/// pools must be rejected by adapter math unless deterministic metadata is
/// explicitly provided.
#[repr(align(64))]
#[derive(Debug, Clone)]
pub struct V4PoolState {
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub liquidity: u128,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub fee_bps: u32, // static LP fee in basis points when hooks are inactive
    pub tick_spacing: i32,
    pub last_update_block: u64,
    pub tick_liquidity_map: HashMap<i32, i128>,
    pub initialized_ticks: Vec<i32>,
    /// Hook contract address for the pool when enabled.
    pub hook_address: Option<Address>,
    /// Canonical hook class label (consumer-provided, validated by adapter).
    pub hook_class: Option<String>,
    // Hook metadata/policy fields for fail-closed behavior.
    pub hooks_enabled: bool,
    pub dynamic_fee_enabled: bool,
    pub deterministic_fee_bps: Option<u32>,
}

impl V4PoolState {
    pub fn new(
        pool_address: Address,
        token0: Address,
        token1: Address,
        fee_bps: u32,
        tick_spacing: i32,
    ) -> Self {
        Self {
            pool_address,
            token0,
            token1,
            liquidity: 0,
            sqrt_price_x96: U256::zero(),
            tick: 0,
            fee_bps,
            tick_spacing,
            last_update_block: 0,
            tick_liquidity_map: HashMap::new(),
            initialized_ticks: Vec::new(),
            hook_address: None,
            hook_class: None,
            hooks_enabled: false,
            dynamic_fee_enabled: false,
            deterministic_fee_bps: None,
        }
    }

    pub fn update_state(
        &mut self,
        liquidity: u128,
        sqrt_price_x96: U256,
        tick: i32,
        block_number: u64,
    ) {
        self.liquidity = liquidity;
        self.sqrt_price_x96 = sqrt_price_x96;
        self.tick = tick;
        self.last_update_block = block_number;
    }
}

/// On-chain fields required by [`curve_math::Pool`] for crypto / NG pools (enriched per block in `pool_manager`).
#[derive(Debug, Clone, Default)]
pub struct CurveMathAux {
    pub crypto_d: Option<U256>,
    pub crypto_gamma: Option<U256>,
    pub crypto_mid_fee: Option<U256>,
    pub crypto_out_fee: Option<U256>,
    pub crypto_fee_gamma: Option<U256>,
    /// Two-coin cryptoswap: `price_scale()` (single word).
    pub price_scale_uni: Option<U256>,
    pub price_scale_0: Option<U256>,
    pub price_scale_1: Option<U256>,
    pub ng_offpeg_fee_multiplier: Option<U256>,
    pub ng_stored_rates: Option<Vec<U256>>,
    /// Base pool `virtual_price()` (1e18-scaled); required for [`curve_math::Pool::StableSwapMeta`] `rates[1]`.
    pub meta_base_virtual_price: Option<U256>,
}

/// Curve pool state (Stableswap)
#[repr(align(64))] // Cache-line aligned
#[derive(Debug, Clone)]
pub struct CurvePoolState {
    pub pool_address: Address,
    pub tokens: Vec<Address>,
    /// Raw on-chain `balances(i)` (native token decimals).
    pub balances: Vec<U256>,
    /// ERC-20 `decimals()` per `tokens` (used to normalize to 18-decimal virtual balances for StableSwap math).
    pub decimals: Vec<u8>,
    pub amplification: U256,
    /// Raw `fee()` word (`fee * 1e10` scale); use for local `get_dy` fee to match on-chain rounding.
    pub fee_raw: U256,
    pub fee_bps: u32,
    /// Per-coin `_stored_rates()` when `stableswap_math_variant` is legacy (sUSD / Compound); `None` for3pool-style (rates from decimals).
    pub stableswap_stored_rates: Option<Vec<U256>>,
    pub stableswap_math_variant: StableswapMathVariant,
    pub last_update_block: u64,
    /// From `pool_resolver/curve_pool_registry.json` — drives quoting and execution policy.
    pub curve_family: CurveFamily,
    pub quoting_strategy: CurveQuotingStrategy,
    pub execution_kind: CurveExecutionKind,
    /// When true, vault/searcher must not build executable Curve legs for this pool.
    pub execution_blocked: bool,
    /// Populated for [`crate::data::curve_registry::CurveQuotingStrategy::LocalCurveMath`] pools.
    pub curve_math_aux: CurveMathAux,
}

impl CurvePoolState {
    pub fn new(pool_address: Address, tokens: Vec<Address>, fee_bps: u32) -> Self {
        let n = tokens.len();
        Self {
            pool_address,
            tokens,
            balances: vec![U256::zero(); n],
            decimals: vec![18u8; n],
            amplification: U256::zero(),
            fee_raw: U256::zero(),
            fee_bps,
            stableswap_stored_rates: None,
            stableswap_math_variant: StableswapMathVariant::Vyper02ThreePool,
            last_update_block: 0,
            curve_family: CurveFamily::StableSwapN,
            quoting_strategy: CurveQuotingStrategy::Unsupported,
            execution_kind: CurveExecutionKind::Unknown,
            execution_blocked: true,
            curve_math_aux: CurveMathAux::default(),
        }
    }

    pub fn apply_registry_entry(&mut self, e: &CurveRegistryEntry) {
        self.curve_family = e.family;
        self.quoting_strategy = e.quoting_strategy;
        self.execution_kind = e.execution_kind;
        self.execution_blocked = e.execution_blocked;
        self.stableswap_math_variant = e.stableswap_math_variant;
    }

    pub fn update_balances(&mut self, balances: Vec<U256>, block_number: u64) {
        self.balances = balances;
        self.last_update_block = block_number;
    }

    pub fn set_decimals(&mut self, decimals: Vec<u8>) {
        self.decimals = decimals;
    }
}

/// Balancer pool state (Weighted pools)
#[repr(align(64))] // Cache-line aligned
#[derive(Debug, Clone)]
pub struct BalancerPoolState {
    pub pool_address: Address,
    pub vault_address: Address,
    pub pool_id: [u8; 32], // Balancer pool ID (bytes32)
    pub tokens: Vec<Address>,
    pub balances: Vec<U256>,
    pub weights: Vec<U256>,
    pub swap_fee_bps: u32,
    pub last_update_block: u64,
    /// `userData` for `IVault.swap` `SingleSwap`. Empty is correct for standard weighted pool swaps.
    /// Stable/composable pools may require non-empty bytes — set when loading pool metadata.
    pub vault_swap_user_data: Vec<u8>,
}

impl BalancerPoolState {
    pub fn new(
        pool_address: Address,
        vault_address: Address,
        pool_id: [u8; 32],
        tokens: Vec<Address>,
        weights: Vec<U256>,
        swap_fee_bps: u32,
    ) -> Self {
        let n = tokens.len();
        Self {
            pool_address,
            vault_address,
            pool_id,
            tokens,
            balances: vec![U256::zero(); n],
            weights,
            swap_fee_bps,
            last_update_block: 0,
            vault_swap_user_data: Vec::new(),
        }
    }

    pub fn update_balances(&mut self, balances: Vec<U256>, block_number: u64) {
        self.balances = balances;
        self.last_update_block = block_number;
    }
}

/// Generic pool state enum
#[derive(Debug, Clone)]
pub enum PoolState {
    V2(V2PoolState),
    V3(V3PoolState),
    V4(V4PoolState),
    Curve(CurvePoolState),
    Balancer(BalancerPoolState),
    Kyber(KyberPoolState),
}

impl PoolState {
    pub fn dex_type(&self) -> DexType {
        match self {
            PoolState::V2(_) => DexType::UniswapV2,
            PoolState::V3(_) => DexType::UniswapV3,
            PoolState::V4(_) => DexType::UniswapV4,
            PoolState::Curve(_) => DexType::Curve,
            PoolState::Balancer(_) => DexType::Balancer,
            PoolState::Kyber(_) => DexType::Kyber,
        }
    }

    pub fn pool_address(&self) -> Address {
        match self {
            PoolState::V2(state) => state.pool_address,
            PoolState::V3(state) => state.pool_address,
            PoolState::V4(state) => state.pool_address,
            PoolState::Curve(state) => state.pool_address,
            PoolState::Balancer(state) => state.pool_address,
            PoolState::Kyber(state) => state.pool_address,
        }
    }

    pub fn last_update_block(&self) -> u64 {
        match self {
            PoolState::V2(state) => state.last_update_block,
            PoolState::V3(state) => state.last_update_block,
            PoolState::V4(state) => state.last_update_block,
            PoolState::Curve(state) => state.last_update_block,
            PoolState::Balancer(state) => state.last_update_block,
            PoolState::Kyber(state) => state.last_update_block,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_pool_state() {
        let mut pool = V2PoolState::new(
            Address::zero(),
            Address::from_low_u64_be(1),
            Address::from_low_u64_be(2),
        );

        pool.update_reserves(U256::from(1000), U256::from(2000), 100);
        assert_eq!(pool.reserve0, U256::from(1000));
        assert_eq!(pool.reserve1, U256::from(2000));
        assert_eq!(pool.last_update_block, 100);
    }

    #[test]
    fn test_v3_pool_state() {
        let mut pool = V3PoolState::new(
            Address::zero(),
            Address::from_low_u64_be(1),
            Address::from_low_u64_be(2),
            3000, // 0.3% fee
        );

        pool.update_state(
            1000000,
            U256::from(79228162514264337593543950336u128),
            0,
            100,
        );
        assert_eq!(pool.liquidity, 1000000);
        assert_eq!(pool.tick, 0);
        assert_eq!(pool.last_update_block, 100);
    }

    #[test]
    fn test_v4_pool_state() {
        let mut pool = V4PoolState::new(
            Address::zero(),
            Address::from_low_u64_be(1),
            Address::from_low_u64_be(2),
            30,
            60,
        );

        pool.update_state(
            777_777,
            U256::from(79228162514264337593543950336u128),
            -12,
            321,
        );
        assert_eq!(pool.liquidity, 777_777);
        assert_eq!(pool.tick, -12);
        assert_eq!(pool.last_update_block, 321);
        assert!(!pool.hooks_enabled);
        assert!(!pool.dynamic_fee_enabled);
    }
}

/// Snapshot of pool states at a specific block
/// Used for sequential simulation where each tx updates state
#[derive(Clone, Debug)]
pub struct BlockStateSnapshot {
    pub block_number: u64,
    pub v2_pools: Vec<(Address, V2PoolState)>,
    pub v3_pools: Vec<(Address, V3PoolState)>,
    pub v4_pools: Vec<(Address, V4PoolState)>,
    pub curve_pools: Vec<(Address, CurvePoolState)>,
    pub balancer_pools: Vec<(Address, BalancerPoolState)>,
    pub kyber_pools: Vec<(Address, KyberPoolState)>,
}
