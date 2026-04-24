use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use alloy_primitives::{address, Address, U256};
use balancer_maths_rust::pools::weighted::weighted_math;
use curve_math::Pool as CurveMathPool;
use dex_math_core_rs::core::BasisPoints;
use dex_math_core_rs::dex::adapter::SwapDirection;
use dex_math_core_rs::dex::balancer::adapter_math::{
    quote_exact_input as balancer_quote_exact_input, BalancerPoolSnapshot,
};
use dex_math_core_rs::dex::curve::adapter_math::{
    quote_exact_input as curve_quote_exact_input, CurvePoolSnapshot,
};
use dex_math_core_rs::dex::uniswap_v2::adapter_math::{
    quote_exact_input as v2_quote_exact_input, V2PoolSnapshot,
};
use dex_math_core_rs::dex::uniswap_v3::adapter_math::{
    quote_exact_input as v3_quote_exact_input, V3PoolSnapshot,
};
use dex_math_core_rs::dex::uniswap_v4::adapter_math::{
    quote_exact_input as v4_quote_exact_input, V4HookMode, V4PoolSnapshot,
};
use hydra_amm::config::{ConstantProductConfig, HybridConfig};
use hydra_amm::domain::{
    Amount as HydraAmount, BasisPoints as HydraBasisPoints, Decimals as HydraDecimals,
    FeeTier as HydraFeeTier, SwapSpec as HydraSwapSpec, Token as HydraToken,
    TokenAddress as HydraTokenAddress, TokenPair as HydraTokenPair,
};
use hydra_amm::pools::{ConstantProductPool, HybridPool};
use hydra_amm::traits::{FromConfig, SwapPool};
use tokio::runtime::Builder;

#[derive(Debug, Clone)]
struct BenchStats {
    ns_per_iter: f64,
    ops_per_sec: f64,
}

#[derive(Debug, Clone)]
struct CompareRow {
    case: &'static str,
    ours: BenchStats,
    competitor: BenchStats,
    speedup_x: f64,
    percent_faster: f64,
}

fn parse_iters() -> u64 {
    let mut args = std::env::args().skip(1);
    let mut iters = 100_000u64;
    while let Some(arg) = args.next() {
        if arg == "--iters" {
            if let Some(v) = args.next() {
                if let Ok(parsed) = v.parse::<u64>() {
                    iters = parsed.max(1);
                }
            }
        }
    }
    iters
}

fn bench_case<F>(iters: u64, mut f: F) -> BenchStats
where
    F: FnMut() -> bool,
{
    let warmup = (iters / 10).clamp(1_000, 20_000);
    for _ in 0..warmup {
        black_box(f());
    }

    let start = Instant::now();
    for _ in 0..iters {
        black_box(f());
    }
    let elapsed_ns = start.elapsed().as_nanos() as f64;
    let ns_per_iter = elapsed_ns / iters as f64;
    let ops_per_sec = 1_000_000_000.0 / ns_per_iter;
    BenchStats {
        ns_per_iter,
        ops_per_sec,
    }
}

fn compare_case<FO, FC>(name: &'static str, iters: u64, mut ours: FO, mut comp: FC) -> CompareRow
where
    FO: FnMut() -> bool,
    FC: FnMut() -> bool,
{
    assert!(ours(), "sanity check failed for ours in case {name}");
    assert!(
        comp(),
        "sanity check failed for competitor in case {name}"
    );

    let ours_stats = bench_case(iters, ours);
    let comp_stats = bench_case(iters, comp);
    let speedup_x = comp_stats.ns_per_iter / ours_stats.ns_per_iter;
    let percent_faster = (1.0 - (ours_stats.ns_per_iter / comp_stats.ns_per_iter)) * 100.0;

    CompareRow {
        case: name,
        ours: ours_stats,
        competitor: comp_stats,
        speedup_x,
        percent_faster,
    }
}

fn one_e18() -> U256 {
    U256::from(10u64).pow(U256::from(18u64))
}

fn v2_uniswap_sdk_case(iters: u64) -> CompareRow {
    let ours_pool = V2PoolSnapshot {
        reserve0: U256::from(1_000_000_000u64),
        reserve1: U256::from(2_000_000_000u64),
        fee_bps: BasisPoints::new_const(30),
    };
    let ours_in = U256::from(10_000u64);

    use uniswap_v2_sdk::prelude as v2sdk;
    let v2_token0 = v2sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000001"),
        18,
        None,
        None,
        0,
        0,
    );
    let v2_token1 = v2sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000002"),
        18,
        None,
        None,
        0,
        0,
    );
    let reserve0 =
        v2sdk::sdk_core::prelude::CurrencyAmount::from_raw_amount(v2_token0.clone(), 1_000_000_000u64)
            .expect("v2 reserve0");
    let reserve1 =
        v2sdk::sdk_core::prelude::CurrencyAmount::from_raw_amount(v2_token1.clone(), 2_000_000_000u64)
            .expect("v2 reserve1");
    let v2_pair = v2sdk::Pair::new(reserve0, reserve1).expect("v2 pair");
    let v2_amount_in = v2sdk::sdk_core::prelude::CurrencyAmount::from_raw_amount(
        v2_token0.clone(),
        10_000u64,
    )
    .expect("v2 input");

    compare_case(
        "uniswap_v2_vs_uniswap-v2-sdk",
        iters,
        || {
            let out = v2_quote_exact_input(&ours_pool, ours_in, SwapDirection::Token0ToToken1)
                .expect("ours v2")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let out = v2_pair
                .get_output_amount(&v2_amount_in, false)
                .expect("sdk v2")
                .0;
            black_box(out);
            true
        },
    )
}

fn v3_uniswap_sdk_case(iters: u64) -> CompareRow {
    let mut v3_liq = std::collections::HashMap::new();
    v3_liq.insert(60, 500_000_000i128);
    v3_liq.insert(120, 0i128);
    let ours_pool = V3PoolSnapshot {
        sqrt_price_x96: dex_math_core_rs::dex::uniswap_v3::math::get_sqrt_ratio_at_tick(0)
            .expect("sqrt"),
        tick: 0,
        liquidity: 1_000_000_000_000u128,
        fee_bps: BasisPoints::new_const(5),
        tick_spacing: 60,
        initialized_ticks: vec![0, 60, 120],
        tick_liquidity_net: v3_liq,
    };
    let ours_in = U256::from(100u64);

    use uniswap_v3_sdk::prelude as v3sdk;
    let v3_token0 = v3sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000001"),
        18,
        None,
        None,
        0,
        0,
    );
    let v3_token1 = v3sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000002"),
        18,
        None,
        None,
        0,
        0,
    );
    let one_ether = 1_000_000_000_000_000_000u128;
    let v3_ticks = v3sdk::TickListDataProvider::new(
        vec![
            v3sdk::Tick::new(
                v3sdk::nearest_usable_tick(v3sdk::MIN_TICK, v3sdk::FeeAmount::LOW.tick_spacing())
                    .as_i32(),
                one_ether,
                one_ether as i128,
            ),
            v3sdk::Tick::new(
                v3sdk::nearest_usable_tick(v3sdk::MAX_TICK, v3sdk::FeeAmount::LOW.tick_spacing())
                    .as_i32(),
                one_ether,
                -(one_ether as i128),
            ),
        ],
        v3sdk::FeeAmount::LOW.tick_spacing().as_i32(),
    );
    let v3_pool = v3sdk::Pool::new_with_tick_data_provider(
        v3_token0.clone(),
        v3_token1.clone(),
        v3sdk::FeeAmount::LOW,
        v3sdk::encode_sqrt_ratio_x96(1, 1),
        one_ether,
        v3_ticks,
    )
    .expect("v3 pool");
    let v3_amount_in = v3sdk::sdk_core::prelude::CurrencyAmount::from_raw_amount(
        v3_token0.clone(),
        100u32,
    )
    .expect("v3 amount in");
    let rt = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    compare_case(
        "uniswap_v3_vs_uniswap-v3-sdk",
        iters,
        || {
            let out = v3_quote_exact_input(&ours_pool, ours_in, SwapDirection::Token0ToToken1)
                .expect("ours v3")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let out = rt
                .block_on(v3_pool.get_output_amount(&v3_amount_in, None))
                .expect("sdk v3");
            black_box(out);
            true
        },
    )
}

fn v4_uniswap_sdk_case(iters: u64) -> CompareRow {
    let mut v4_liq = std::collections::HashMap::new();
    v4_liq.insert(60, -200_000i128);
    v4_liq.insert(120, -150_000i128);
    let ours_pool = V4PoolSnapshot {
        hook_address: None,
        hook_class: None,
        sqrt_price_x96: dex_math_core_rs::dex::kyber::math::tick_math::get_sqrt_ratio_at_tick(0)
            .expect("sqrt"),
        tick: 0,
        liquidity: 1_000_000_000_000u128,
        fee_bps: BasisPoints::new_const(5),
        tick_spacing: 60,
        initialized_ticks: vec![-120, -60, 0, 60, 120],
        tick_liquidity_net: v4_liq,
        hook_mode: V4HookMode::NoHooks,
    };
    let ours_in = U256::from(100u64);

    use uniswap_v4_sdk::prelude as v4sdk;
    let v4_token0 = v4sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000001"),
        18,
        None,
        None,
        0,
        0,
    );
    let v4_token1 = v4sdk::sdk_core::prelude::Token::new(
        1,
        address!("0000000000000000000000000000000000000002"),
        18,
        None,
        None,
        0,
        0,
    );
    let one_ether = 1_000_000_000_000_000_000u128;
    let tick_spacing = 10i32;
    let v4_ticks = vec![
        v4sdk::v3_sdk::prelude::Tick::new(
            v4sdk::v3_sdk::prelude::nearest_usable_tick(
                v4sdk::v3_sdk::prelude::MIN_TICK.as_i32(),
                tick_spacing,
            ),
            one_ether,
            one_ether as i128,
        ),
        v4sdk::v3_sdk::prelude::Tick::new(
            v4sdk::v3_sdk::prelude::nearest_usable_tick(
                v4sdk::v3_sdk::prelude::MAX_TICK.as_i32(),
                tick_spacing,
            ),
            one_ether,
            -(one_ether as i128),
        ),
    ];
    let v4_pool = v4sdk::Pool::new_with_tick_data_provider(
        v4sdk::sdk_core::prelude::Currency::Token(v4_token0.clone()),
        v4sdk::sdk_core::prelude::Currency::Token(v4_token1.clone()),
        v4sdk::v3_sdk::prelude::FeeAmount::LOWEST.into(),
        tick_spacing,
        Address::ZERO,
        v4sdk::v3_sdk::prelude::encode_sqrt_ratio_x96(1, 1),
        one_ether,
        v4_ticks,
    )
    .expect("v4 pool");
    let v4_amount_in = v4sdk::sdk_core::prelude::CurrencyAmount::from_raw_amount(
        v4sdk::sdk_core::prelude::Currency::Token(v4_token0.clone()),
        100u32,
    )
    .expect("v4 amount in");
    let rt = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    compare_case(
        "uniswap_v4_hookless_vs_uniswap-v4-sdk",
        iters,
        || {
            let out = v4_quote_exact_input(&ours_pool, ours_in, SwapDirection::Token0ToToken1)
                .expect("ours v4")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let out = rt
                .block_on(v4_pool.get_output_amount(&v4_amount_in, None))
                .expect("sdk v4")
                .0;
            black_box(out);
            true
        },
    )
}

fn balancer_canonical_case(iters: u64) -> CompareRow {
    let ours_pool = BalancerPoolSnapshot {
        balance0: U256::from(1_000_000_000u64),
        balance1: U256::from(2_000_000_000u64),
        weight0: one_e18() / U256::from(2u64),
        weight1: one_e18() / U256::from(2u64),
        swap_fee_bps: BasisPoints::new_const(30),
    };
    let ours_in = U256::from(10_000u64);
    let fee = BasisPoints::new_const(30);
    let net_in = ours_in - ((ours_in * U256::from(fee.as_u32())) / U256::from(10_000u64));

    compare_case(
        "balancer_weighted_vs_balancer-maths-rust",
        iters,
        || {
            let out = balancer_quote_exact_input(&ours_pool, ours_in, SwapDirection::Token0ToToken1)
                .expect("ours balancer")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let out = weighted_math::compute_out_given_exact_in(
                &U256::from(1_000_000_000u64),
                &(one_e18() / U256::from(2u64)),
                &U256::from(2_000_000_000u64),
                &(one_e18() / U256::from(2u64)),
                &net_in,
            )
            .expect("balancer canonical");
            black_box(out);
            true
        },
    )
}

fn curve_canonical_case(iters: u64) -> CompareRow {
    let ours_pool = CurvePoolSnapshot {
        balances: vec![U256::from(1_000_000_000_000u64), U256::from(1_000_000_000_000u64)],
        decimals: vec![18, 18],
        stored_rates: None,
        precomputed_rates: None,
        variant: dex_math_core_rs::data::curve_registry::StableswapMathVariant::Vyper02ThreePool,
        amplification: U256::from(1000u64),
        fee_raw: U256::ZERO,
        fee_bps: 4,
    };
    let ours_in = U256::from(1_000_000u64);

    let curve_pool = CurveMathPool::StableSwapV2 {
        balances: vec![U256::from(1_000_000_000_000u64), U256::from(1_000_000_000_000u64)],
        rates: vec![one_e18(), one_e18()],
        amp: U256::from(40_000u64),
        fee: U256::from(4_000_000u64),
    };

    compare_case(
        "curve_adapter_vs_curve-math",
        (iters / 4).max(10_000),
        || {
            let out = curve_quote_exact_input(&ours_pool, 0, 1, ours_in)
                .expect("ours curve")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let out = curve_pool.get_amount_out(0, 1, U256::from(1_000_000u64)).expect("curve");
            black_box(out);
            true
        },
    )
}

fn hydra_constant_product_case(iters: u64) -> CompareRow {
    let ours_pool = V2PoolSnapshot {
        reserve0: U256::from(1_000_000_000u64),
        reserve1: U256::from(2_000_000_000u64),
        fee_bps: BasisPoints::new_const(30),
    };
    let ours_in = U256::from(10_000u64);

    let decimals = HydraDecimals::new(18).expect("hydra decimals");
    let token_a = HydraToken::new(HydraTokenAddress::from_bytes([1u8; 32]), decimals);
    let token_b = HydraToken::new(HydraTokenAddress::from_bytes([2u8; 32]), decimals);
    let pair = HydraTokenPair::new(token_a, token_b).expect("hydra pair");
    let fee_tier = HydraFeeTier::new(HydraBasisPoints::new(30));
    let cfg = ConstantProductConfig::new(
        pair,
        fee_tier,
        HydraAmount::new(1_000_000_000u128),
        HydraAmount::new(2_000_000_000u128),
    )
    .expect("hydra cp cfg");
    let hydra_pool = ConstantProductPool::from_config(&cfg).expect("hydra cp pool");
    let swap_spec = HydraSwapSpec::exact_in(HydraAmount::new(10_000u128)).expect("hydra swap");

    compare_case(
        "uniswap_v2_vs_hydra_constant_product",
        iters,
        || {
            let out = v2_quote_exact_input(&ours_pool, ours_in, SwapDirection::Token0ToToken1)
                .expect("ours v2")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let mut pool = hydra_pool.clone();
            let out = pool.swap(swap_spec, token_a).expect("hydra cp").amount_out();
            let _ = black_box(out);
            true
        },
    )
}

fn hydra_hybrid_case(iters: u64) -> CompareRow {
    let ours_pool = CurvePoolSnapshot {
        balances: vec![U256::from(1_000_000_000_000u64), U256::from(1_000_000_000_000u64)],
        decimals: vec![18, 18],
        stored_rates: None,
        precomputed_rates: None,
        variant: dex_math_core_rs::data::curve_registry::StableswapMathVariant::Vyper02ThreePool,
        amplification: U256::from(1000u64),
        fee_raw: U256::ZERO,
        fee_bps: 4,
    };
    let ours_in = U256::from(1_000_000u64);

    let decimals = HydraDecimals::new(18).expect("hydra decimals");
    let token_a = HydraToken::new(HydraTokenAddress::from_bytes([3u8; 32]), decimals);
    let token_b = HydraToken::new(HydraTokenAddress::from_bytes([4u8; 32]), decimals);
    let pair = HydraTokenPair::new(token_a, token_b).expect("hydra pair");
    let fee_tier = HydraFeeTier::new(HydraBasisPoints::new(4));
    let cfg = HybridConfig::new(
        pair,
        fee_tier,
        100,
        HydraAmount::new(1_000_000_000_000u128),
        HydraAmount::new(1_000_000_000_000u128),
    )
    .expect("hydra hybrid cfg");
    let hydra_pool = HybridPool::from_config(&cfg).expect("hydra hybrid pool");
    let swap_spec = HydraSwapSpec::exact_in(HydraAmount::new(1_000_000u128)).expect("hydra swap");

    compare_case(
        "curve_adapter_vs_hydra_hybrid",
        (iters / 4).max(10_000),
        || {
            let out = curve_quote_exact_input(&ours_pool, 0, 1, ours_in)
                .expect("ours curve")
                .amount_out;
            black_box(out);
            true
        },
        || {
            let mut pool = hydra_pool.clone();
            let out = pool
                .swap(swap_spec, token_a)
                .expect("hydra hybrid")
                .amount_out();
            let _ = black_box(out);
            true
        },
    )
}

fn write_markdown_report(rows: &[CompareRow], iters: u64) -> std::io::Result<PathBuf> {
    let mut out = String::new();
    writeln!(&mut out, "# Competitor Benchmark Report").ok();
    writeln!(&mut out).ok();
    writeln!(&mut out, "- Iterations: `{iters}` (per primary case)").ok();
    writeln!(
        &mut out,
        "- Method: release-mode microbench with warmup and deterministic fixtures."
    )
    .ok();
    writeln!(
        &mut out,
        "- Policy: fail-closed paths only; no synthetic fallback quotes."
    )
    .ok();
    writeln!(&mut out).ok();

    writeln!(
        &mut out,
        "| Case | Ours ns/op | Competitor ns/op | Ours ops/s | Competitor ops/s | Speedup (x) | Faster (%) |"
    )
    .ok();
    writeln!(
        &mut out,
        "|---|---:|---:|---:|---:|---:|---:|"
    )
    .ok();
    for row in rows {
        writeln!(
            &mut out,
            "| {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.3} | {:.2} |",
            row.case,
            row.ours.ns_per_iter,
            row.competitor.ns_per_iter,
            row.ours.ops_per_sec,
            row.competitor.ops_per_sec,
            row.speedup_x,
            row.percent_faster
        )
        .ok();
    }

    let report_path = PathBuf::from("benches").join("competitor_report.md");
    fs::create_dir_all("benches")?;
    fs::write(&report_path, out)?;
    Ok(report_path)
}

fn main() {
    let iters = parse_iters();
    let rows = vec![
        v2_uniswap_sdk_case(iters),
        v3_uniswap_sdk_case((iters / 2).max(25_000)),
        v4_uniswap_sdk_case((iters / 2).max(25_000)),
        balancer_canonical_case(iters),
        curve_canonical_case(iters),
        hydra_constant_product_case(iters),
        hydra_hybrid_case(iters),
    ];

    let report = write_markdown_report(&rows, iters).expect("write report");
    println!("Wrote markdown report: {}", report.display());
}
