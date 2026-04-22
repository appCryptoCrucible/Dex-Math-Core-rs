//! Embedded Curve pool registry (taxonomy, quoting, execution policy).
//!
//! Source of truth: [pool_resolver/curve_pool_registry.json](../../pool_resolver/curve_pool_registry.json)

use ethers_core::types::{Address, U256};
use serde::Deserialize;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::OnceLock;

static REGISTRY: OnceLock<CurvePoolRegistry> = OnceLock::new();

/// JSON embedded at compile time — keep in sync with `pool_resolver/curve_pool_registry.json`.
const EMBEDDED_REGISTRY_JSON: &str = include_str!("../../pool_resolver/curve_pool_registry.json");

/// Which Curve Vyper `get_D` / `get_dy` integer semantics apply (must match the deployed pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub enum StableswapMathVariant {
    /// `StableSwap3Pool.vy` (0.2.x): `D_P` step divides by `x * N`; `get_dy` uses `(xp[j]-y-1)`.
    #[serde(rename = "vyper_0_2_three_pool", alias = "vyper02_three_pool")]
    #[default]
    Vyper02ThreePool,
    /// `StableSwapSUSD.vy` / `StableSwapUSDT.vy` (0.1.x): `D_P` divides by `x * N + 1`; `get_dy` uses `(xp[j]-y)` (no −1).
    #[serde(rename = "vyper_0_1_legacy", alias = "vyper01_legacy")]
    Vyper01Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum CurveFamily {
    #[serde(rename = "StableSwapN")]
    StableSwapN,
    #[serde(rename = "CryptoTricrypto")]
    CryptoTricrypto,
    #[serde(rename = "MetaStableSwap")]
    MetaStableSwap,
    #[serde(rename = "TwoCryptoNg")]
    TwoCryptoNg,
    #[serde(rename = "StableSwapNg")]
    StableSwapNg,
    /// Plain StableSwap-style pools (factory / crvUSD markets) using `curve-math` StableSwapV2 — not NG dynamic-fee ABI.
    #[serde(rename = "StableSwapV2")]
    StableSwapV2,
    #[serde(rename = "LsdEthPool")]
    LsdEthPool,
    #[serde(rename = "VolatileAmm")]
    VolatileAmm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum CurveQuotingStrategy {
    /// Use `crate::dex::curve::math` StableSwap path (Phase 2 parity required).
    #[serde(rename = "AnalyticStableSwap")]
    AnalyticStableSwap,
    /// Use on-chain `get_dy` / `get_dy_underlying` at pinned block.
    #[serde(rename = "OnChainView")]
    OnChainView,
    /// Wei-exact local quote via `curve-math` + enriched pool snapshot (see `CurveMathAux`).
    #[serde(rename = "LocalCurveMath")]
    LocalCurveMath,
    /// No quoting — strategy must skip.
    #[serde(rename = "Unsupported")]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveExecutionKind {
    ExchangeInt128,
    ExchangeUint256,
    ExchangeUnderlyingInt128,
    NotYetSupported,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurveFeeEncoding {
    /// `fee()` returns Curve1e10 fixed-point; swap fee fraction = fee/1e10; bps = fee / 1e6.
    #[serde(alias = "curve_fee_1e10")]
    CurveFee1e10,
    /// Tricrypto / crypto pools — do not use for local StableSwap fee decode; prefer on-chain quote.
    TricryptoMidFee,
    UnknownNotForLocalMath,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurveRegistryEntry {
    pub name: String,
    pub address: String,
    pub family: CurveFamily,
    pub n_coins: u8,
    pub quoting_strategy: CurveQuotingStrategy,
    pub execution_kind: CurveExecutionKind,
    pub fee_encoding: CurveFeeEncoding,
    #[serde(default)]
    pub execution_blocked: bool,
    /// StableSwap integer math variant; default `Vyper02ThreePool` when omitted.
    #[serde(default)]
    pub stableswap_math_variant: StableswapMathVariant,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub verification_refs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryFile {
    #[allow(dead_code)]
    schema_version: u32,
    pools: Vec<CurveRegistryEntry>,
}

#[derive(Debug, Clone)]
pub struct CurvePoolRegistry {
    by_address: HashMap<Address, CurveRegistryEntry>,
}

impl CurvePoolRegistry {
    pub fn from_embedded_json() -> Result<Self, String> {
        let file: RegistryFile =
            serde_json::from_str(EMBEDDED_REGISTRY_JSON).map_err(|e| e.to_string())?;
        let mut by_address = HashMap::with_capacity(file.pools.len());
        for p in file.pools {
            let addr = Address::from_str(p.address.trim())
                .map_err(|e| format!("bad address {}: {}", p.address, e))?;
            by_address.insert(addr, p);
        }
        Ok(Self { by_address })
    }

    pub fn global() -> &'static CurvePoolRegistry {
        REGISTRY.get_or_init(|| {
            CurvePoolRegistry::from_embedded_json()
                .expect("embedded curve_pool_registry.json must parse")
        })
    }

    pub fn get(&self, pool: &Address) -> Option<&CurveRegistryEntry> {
        self.by_address.get(pool)
    }

    pub fn require(&self, pool: &Address) -> Result<&CurveRegistryEntry, String> {
        self.get(pool)
            .ok_or_else(|| format!("Curve pool {:?} not in curve_pool_registry.json", pool))
    }

    pub fn all_pool_addresses(&self) -> Vec<Address> {
        self.by_address.keys().copied().collect()
    }

    /// Pools allowed for vault execution per registry (still subject to env allowlist + encoder checks).
    pub fn execution_unblocked_addresses(&self) -> Vec<Address> {
        self.by_address
            .iter()
            .filter(|(_, e)| !e.execution_blocked)
            .map(|(a, _)| *a)
            .collect()
    }
}

/// Decode `fee()` return word to basis points per registry policy. No silent defaults.
pub fn decode_curve_fee_bps(fee_raw: U256, enc: CurveFeeEncoding) -> Result<u32, String> {
    if fee_raw.is_zero() {
        return Err("Curve fee() returned zero — refusing guessed fee".to_string());
    }
    match enc {
        CurveFeeEncoding::CurveFee1e10 => {
            // bps = fee_raw * 10000 / 1e10 = fee_raw / 1e6
            let bps_u128: u128 = fee_raw
                .checked_div(U256::from(1_000_000u64))
                .ok_or_else(|| "fee division failed".to_string())?
                .as_u128();
            if bps_u128 == 0 || bps_u128 > 500 {
                return Err(format!(
                    "decoded fee bps {} out of sane range (1-500) for curve_fee_1e10",
                    bps_u128
                ));
            }
            Ok(bps_u128 as u32)
        }
        CurveFeeEncoding::TricryptoMidFee | CurveFeeEncoding::UnknownNotForLocalMath => Err(
            "fee_encoding does not support local bps decode — use OnChainView quoting".to_string(),
        ),
    }
}

pub fn curve_registry() -> &'static CurvePoolRegistry {
    CurvePoolRegistry::global()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ethers_core::types::U256;

    #[test]
    fn embedded_registry_parses_and_has_expected_pool_count() {
        let r = CurvePoolRegistry::from_embedded_json().expect("parse");
        assert_eq!(r.all_pool_addresses().len(), 16);
    }

    #[test]
    fn decode_fee_4bps_from_4e6() {
        let bps = decode_curve_fee_bps(U256::from(4_000_000u64), CurveFeeEncoding::CurveFee1e10)
            .expect("bps");
        assert_eq!(bps, 4);
    }
}
