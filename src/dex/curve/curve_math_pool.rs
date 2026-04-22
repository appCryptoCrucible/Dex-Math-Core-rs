//! Bridge [`CurvePoolState`](crate::data::pool_state::CurvePoolState) → [`curve_math::Pool`] for
//! wei-exact local quotes (`curve-math` crate is differential-fuzzed vs on-chain `get_dy`).

use crate::core::{DexError, MathError};
use crate::data::curve_registry::{CurveFamily, CurveRegistryEntry, StableswapMathVariant};
use crate::data::pool_state::CurvePoolState;
use crate::dex::common::{alloy_to_ethers, ethers_to_alloy};
use crate::dex::curve::math::stableswap_rate_for_decimals;
use alloy_primitives::U256 as A256;
use curve_math::Pool;
use ethers_core::types::U256;

#[inline]
fn a256_from_ethers(u: U256) -> A256 {
    ethers_to_alloy(u)
}

#[inline]
fn ethers_from_a256(u: A256) -> U256 {
    alloy_to_ethers(u)
}

/// Cryptoswap `precisions[i] = 10**(18 - decimals[i])` (matches `curve-math` fuzz tests).
#[inline]
fn crypto_precision_10_pow_18_minus_dec(dec: u8) -> Result<A256, DexError> {
    if dec > 18 {
        return Err(DexError::MathError(MathError::InvalidInput {
            operation: "crypto_precision".to_string(),
            reason: format!("decimals {} > 18", dec),
            context: "Curve cryptoswap precisions".to_string(),
        }));
    }
    let exp = 18u32 - u32::from(dec);
    let mut acc = A256::ONE;
    let ten = A256::from(10u64);
    for _ in 0..exp {
        acc = acc.checked_mul(ten).ok_or_else(|| {
            DexError::MathError(MathError::Overflow {
                operation: "crypto_precision".to_string(),
                inputs: vec![alloy_to_ethers(acc), alloy_to_ethers(ten)],
                context: format!("10^{}", exp),
            })
        })?;
    }
    Ok(acc)
}

/// Which [`curve_math::Pool`] template applies (factory / deployment family).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveMathTemplate {
    StableSwapV0,
    StableSwapV1,
    StableSwapV2,
    StableSwapNG,
    StableSwapMeta,
    TwoCryptoV1,
    TwoCryptoNG,
    TriCryptoV1,
}

pub fn curve_math_template(entry: &CurveRegistryEntry) -> Result<CurveMathTemplate, DexError> {
    match entry.family {
        CurveFamily::LsdEthPool | CurveFamily::StableSwapV2 => Ok(CurveMathTemplate::StableSwapV2),
        CurveFamily::StableSwapNg => Ok(CurveMathTemplate::StableSwapNG),
        CurveFamily::CryptoTricrypto => Ok(CurveMathTemplate::TriCryptoV1),
        CurveFamily::TwoCryptoNg => {
            if entry.name == "CVX_ETH" {
                Ok(CurveMathTemplate::TwoCryptoV1)
            } else {
                Ok(CurveMathTemplate::TwoCryptoNG)
            }
        }
        CurveFamily::MetaStableSwap => Ok(CurveMathTemplate::StableSwapMeta),
        CurveFamily::StableSwapN => match entry.stableswap_math_variant {
            StableswapMathVariant::Vyper01Legacy => Ok(CurveMathTemplate::StableSwapV0),
            StableswapMathVariant::Vyper02ThreePool => Ok(CurveMathTemplate::StableSwapV1),
        },
        CurveFamily::VolatileAmm => Err(DexError::InvalidPool {
            reason: format!(
                "curve_math_pool: family {:?} not supported for LocalCurveMath (pool {})",
                entry.family, entry.name
            ),
        }),
    }
}

fn stable_rates_from_decimals(decimals: &[u8]) -> Result<Vec<A256>, DexError> {
    let mut v = Vec::with_capacity(decimals.len());
    for &d in decimals {
        v.push(stableswap_rate_for_decimals(d).map_err(DexError::MathError)?);
    }
    Ok(v)
}

fn require_aux_u256(aux: Option<U256>, field: &'static str) -> Result<U256, DexError> {
    aux.ok_or_else(|| DexError::InvalidPool {
        reason: format!("Curve local math: missing enriched field {}", field),
    })
}

/// Build a [`Pool`] snapshot for [`crate::data::curve_registry::CurveQuotingStrategy::LocalCurveMath`].
pub fn curve_math_pool_from_state(state: &CurvePoolState) -> Result<Pool, DexError> {
    let entry = crate::data::curve_registry::curve_registry()
        .require(&state.pool_address)
        .map_err(|e| DexError::InvalidPool { reason: e })?;
    let tmpl = curve_math_template(entry)?;

    let fee_a = a256_from_ethers(state.fee_raw);
    let raw_amp = state.amplification;
    const AP100: u64 = 100;

    match tmpl {
        CurveMathTemplate::StableSwapV0 | CurveMathTemplate::StableSwapV1 => {
            let rates: Vec<A256> = if let Some(ref sr) = state.stableswap_stored_rates {
                if sr.len() != state.balances.len() {
                    return Err(DexError::InvalidPool {
                        reason: "StableSwapV0/V1: stableswap_stored_rates length mismatch".to_string(),
                    });
                }
                sr.iter().copied().map(a256_from_ethers).collect()
            } else {
                stable_rates_from_decimals(&state.decimals)?
            };
            let balances: Vec<A256> = state.balances.iter().copied().map(a256_from_ethers).collect();
            if balances.len() != rates.len() {
                return Err(DexError::InvalidPool {
                    reason: "StableSwapV0/V1: balances/rates length mismatch".to_string(),
                });
            }
            let amp = a256_from_ethers(raw_amp);
            match tmpl {
                CurveMathTemplate::StableSwapV0 => Ok(Pool::StableSwapV0 {
                    balances,
                    rates,
                    amp,
                    fee: fee_a,
                }),
                CurveMathTemplate::StableSwapV1 => Ok(Pool::StableSwapV1 {
                    balances,
                    rates,
                    amp,
                    fee: fee_a,
                }),
                _ => unreachable!(),
            }
        }
        CurveMathTemplate::StableSwapV2 => {
            let rates = stable_rates_from_decimals(&state.decimals)?;
            let balances: Vec<A256> = state.balances.iter().copied().map(a256_from_ethers).collect();
            if balances.len() != rates.len() {
                return Err(DexError::InvalidPool {
                    reason: "StableSwapV2: balances/rates length mismatch".to_string(),
                });
            }
            let amp = raw_amp
                .checked_mul(U256::from(AP100))
                .ok_or_else(|| DexError::InvalidPool {
                    reason: "StableSwapV2: amp * A_PRECISION overflow".to_string(),
                })?;
            Ok(Pool::StableSwapV2 {
                balances,
                rates,
                amp: a256_from_ethers(amp),
                fee: fee_a,
            })
        }
        CurveMathTemplate::StableSwapMeta => {
            let aux = &state.curve_math_aux;
            let vp = require_aux_u256(aux.meta_base_virtual_price, "meta_base_virtual_price")?;
            if state.balances.len() != 2 || state.decimals.len() != 2 {
                return Err(DexError::InvalidPool {
                    reason: "StableSwapMeta: expected 2 coins".to_string(),
                });
            }
            let r0 = stableswap_rate_for_decimals(state.decimals[0]).map_err(DexError::MathError)?;
            let rates = vec![r0, a256_from_ethers(vp)];
            let balances: Vec<A256> = state.balances.iter().copied().map(a256_from_ethers).collect();
            let amp = raw_amp
                .checked_mul(U256::from(AP100))
                .ok_or_else(|| DexError::InvalidPool {
                    reason: "StableSwapMeta: amp * A_PRECISION overflow".to_string(),
                })?;
            Ok(Pool::StableSwapMeta {
                balances,
                rates,
                amp: a256_from_ethers(amp),
                fee: fee_a,
            })
        }
        CurveMathTemplate::StableSwapNG => {
            let aux = &state.curve_math_aux;
            let offpeg = require_aux_u256(aux.ng_offpeg_fee_multiplier, "offpeg_fee_multiplier")?;
            let rates: Vec<A256> = if let Some(ref sr) = aux.ng_stored_rates {
                if sr.len() != state.balances.len() {
                    return Err(DexError::InvalidPool {
                        reason: "StableSwapNG: stored_rates length mismatch".to_string(),
                    });
                }
                sr.iter().copied().map(a256_from_ethers).collect()
            } else {
                stable_rates_from_decimals(&state.decimals)?
            };
            let balances: Vec<A256> = state.balances.iter().copied().map(a256_from_ethers).collect();
            let amp = raw_amp
                .checked_mul(U256::from(AP100))
                .ok_or_else(|| DexError::InvalidPool {
                    reason: "StableSwapNG: amp * A_PRECISION overflow".to_string(),
                })?;
            Ok(Pool::StableSwapNG {
                balances,
                rates,
                amp: a256_from_ethers(amp),
                fee: fee_a,
                offpeg_fee_multiplier: a256_from_ethers(offpeg),
            })
        }
        CurveMathTemplate::TwoCryptoV1 | CurveMathTemplate::TwoCryptoNG => {
            let aux = &state.curve_math_aux;
            let d = require_aux_u256(aux.crypto_d, "D")?;
            let gamma = require_aux_u256(aux.crypto_gamma, "gamma")?;
            let mid_fee = require_aux_u256(aux.crypto_mid_fee, "mid_fee")?;
            let out_fee = require_aux_u256(aux.crypto_out_fee, "out_fee")?;
            let fee_gamma = require_aux_u256(aux.crypto_fee_gamma, "fee_gamma")?;
            let ps = require_aux_u256(aux.price_scale_uni, "price_scale")?;
            if state.balances.len() != 2 || state.decimals.len() != 2 {
                return Err(DexError::InvalidPool {
                    reason: "TwoCrypto: expected 2 coins".to_string(),
                });
            }
            let balances = [
                a256_from_ethers(state.balances[0]),
                a256_from_ethers(state.balances[1]),
            ];
            let precisions = [
                crypto_precision_10_pow_18_minus_dec(state.decimals[0])?,
                crypto_precision_10_pow_18_minus_dec(state.decimals[1])?,
            ];
            let ann = raw_amp;
            match tmpl {
                CurveMathTemplate::TwoCryptoV1 => Ok(Pool::TwoCryptoV1 {
                    balances,
                    precisions,
                    price_scale: a256_from_ethers(ps),
                    d: a256_from_ethers(d),
                    ann: a256_from_ethers(ann),
                    gamma: a256_from_ethers(gamma),
                    mid_fee: a256_from_ethers(mid_fee),
                    out_fee: a256_from_ethers(out_fee),
                    fee_gamma: a256_from_ethers(fee_gamma),
                }),
                CurveMathTemplate::TwoCryptoNG => Ok(Pool::TwoCryptoNG {
                    balances,
                    precisions,
                    price_scale: a256_from_ethers(ps),
                    d: a256_from_ethers(d),
                    ann: a256_from_ethers(ann),
                    gamma: a256_from_ethers(gamma),
                    mid_fee: a256_from_ethers(mid_fee),
                    out_fee: a256_from_ethers(out_fee),
                    fee_gamma: a256_from_ethers(fee_gamma),
                }),
                _ => unreachable!(),
            }
        }
        CurveMathTemplate::TriCryptoV1 => {
            let aux = &state.curve_math_aux;
            let d = require_aux_u256(aux.crypto_d, "D")?;
            let gamma = require_aux_u256(aux.crypto_gamma, "gamma")?;
            let mid_fee = require_aux_u256(aux.crypto_mid_fee, "mid_fee")?;
            let out_fee = require_aux_u256(aux.crypto_out_fee, "out_fee")?;
            let fee_gamma = require_aux_u256(aux.crypto_fee_gamma, "fee_gamma")?;
            let ps0 = require_aux_u256(aux.price_scale_0, "price_scale(0)")?;
            let ps1 = require_aux_u256(aux.price_scale_1, "price_scale(1)")?;
            if state.balances.len() != 3 || state.decimals.len() != 3 {
                return Err(DexError::InvalidPool {
                    reason: "TriCrypto: expected 3 coins".to_string(),
                });
            }
            let balances = [
                a256_from_ethers(state.balances[0]),
                a256_from_ethers(state.balances[1]),
                a256_from_ethers(state.balances[2]),
            ];
            let precisions = [
                crypto_precision_10_pow_18_minus_dec(state.decimals[0])?,
                crypto_precision_10_pow_18_minus_dec(state.decimals[1])?,
                crypto_precision_10_pow_18_minus_dec(state.decimals[2])?,
            ];
            Ok(Pool::TriCryptoV1 {
                balances,
                precisions,
                price_scale: [a256_from_ethers(ps0), a256_from_ethers(ps1)],
                d: a256_from_ethers(d),
                ann: a256_from_ethers(raw_amp),
                gamma: a256_from_ethers(gamma),
                mid_fee: a256_from_ethers(mid_fee),
                out_fee: a256_from_ethers(out_fee),
                fee_gamma: a256_from_ethers(fee_gamma),
            })
        }
    }
}

/// Local wei-exact output for `dx` of pool **coin** `i` → `j` (matches on-chain `get_dy`, not `get_dy_underlying`).
pub fn curve_math_quote_out(state: &CurvePoolState, i: usize, j: usize, dx: U256) -> Result<U256, DexError> {
    let pool = curve_math_pool_from_state(state)?;
    let dy_a = pool
        .get_amount_out(i, j, a256_from_ethers(dx))
        .ok_or_else(|| DexError::InvalidPool {
            reason: "curve-math get_amount_out returned None (zero dx or infeasible swap)".to_string(),
        })?;
    Ok(ethers_from_a256(dy_a))
}
