use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use alloy_primitives::U256;
use dex_math_core_rs::core::BasisPoints;
use dex_math_core_rs::dex::adapter::SwapDirection;
use dex_math_core_rs::dex::balancer::adapter_math::{
    quote_exact_input as balancer_quote_exact_input, BalancerPoolSnapshot,
};
use dex_math_core_rs::dex::curve::adapter_math::{
    quote_exact_input as curve_quote_exact_input, CurvePoolSnapshot,
};
use dex_math_core_rs::dex::kyber::adapter_math::{
    quote_exact_input as kyber_quote_exact_input, KyberPoolSnapshot,
};
use dex_math_core_rs::dex::kyber::math::tick_math;
use dex_math_core_rs::dex::uniswap_v3::adapter_math::{
    quote_exact_input as v3_quote_exact_input, V3PoolSnapshot,
};
use dex_math_core_rs::dex::uniswap_v4::adapter_math::{
    quote_exact_input as v4_quote_exact_input, V4HookMode, V4PoolSnapshot,
};

#[derive(Debug)]
struct BenchResult {
    name: &'static str,
    iters: u64,
    total_ns: u128,
    ns_per_iter: f64,
    ops_per_sec: f64,
    checksum: u64,
}

fn parse_iters() -> u64 {
    let mut args = std::env::args().skip(1);
    let mut iters = 200_000u64;
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

fn bench_case<F>(name: &'static str, iters: u64, mut f: F) -> BenchResult
where
    F: FnMut() -> U256,
{
    let warmup = (iters / 10).clamp(1_000, 20_000);
    let mut sink = 0u64;
    for _ in 0..warmup {
        let out = f();
        sink ^= out.as_limbs()[0];
    }
    black_box(sink);

    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..iters {
        let out = black_box(f());
        checksum ^= out.as_limbs()[0];
    }
    let elapsed = start.elapsed();
    let total_ns = elapsed.as_nanos();
    let ns_per_iter = total_ns as f64 / iters as f64;
    let ops_per_sec = 1_000_000_000f64 / ns_per_iter;

    BenchResult {
        name,
        iters,
        total_ns,
        ns_per_iter,
        ops_per_sec,
        checksum,
    }
}

fn w(percent: u64) -> U256 {
    U256::from(percent) * U256::from(10u64).pow(U256::from(16u64))
}

fn fixture_v3_cross() -> (V3PoolSnapshot, U256, SwapDirection) {
    let mut liq = HashMap::new();
    liq.insert(60, 500_000_000i128);
    liq.insert(120, 0i128);
    let sqrt_59 = dex_math_core_rs::dex::uniswap_v3::math::get_sqrt_ratio_at_tick(59).unwrap();
    let sqrt_60 = dex_math_core_rs::dex::uniswap_v3::math::get_sqrt_ratio_at_tick(60).unwrap();
    let max_to_60 =
        dex_math_core_rs::dex::uniswap_v3::math::get_amount1_delta(sqrt_59, sqrt_60, 1_000_000_000u128, true)
            .unwrap();
    (
        V3PoolSnapshot {
            sqrt_price_x96: sqrt_59,
            tick: 59,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(300),
            tick_spacing: 60,
            initialized_ticks: vec![0, 60, 120],
            tick_liquidity_net: liq,
        },
        max_to_60 * U256::from(2u64),
        SwapDirection::Token1ToToken0,
    )
}

fn fixture_kyber_cross() -> (KyberPoolSnapshot, U256, SwapDirection) {
    let mut liq = HashMap::new();
    liq.insert(60, 100_000_000i128);
    liq.insert(120, 0i128);
    liq.insert(180, 0i128);

    let sqrt_price_x96 = tick_math::get_sqrt_ratio_at_tick(59).unwrap();
    let sqrt_60 = tick_math::get_sqrt_ratio_at_tick(60).unwrap();
    let max_to_60 =
        dex_math_core_rs::dex::uniswap_v3::math::get_amount1_delta(sqrt_price_x96, sqrt_60, 1_000_000_000u128, true)
            .unwrap();

    (
        KyberPoolSnapshot {
            sqrt_price_x96,
            tick: 59,
            liquidity: 1_000_000_000u128,
            fee_bps: BasisPoints::new_const(25),
            tick_spacing: 60,
            tick_bitmap_words: HashMap::new(),
            initialized_ticks: vec![-60, 0, 60, 120],
            tick_liquidity_net: liq,
        },
        max_to_60 * U256::from(2u64),
        SwapDirection::Token1ToToken0,
    )
}

fn fixture_v4_cross() -> (V4PoolSnapshot, U256, SwapDirection) {
    (
        V4PoolSnapshot {
            hook_address: None,
            hook_class: None,
            sqrt_price_x96: tick_math::get_sqrt_ratio_at_tick(0).unwrap(),
            tick: 0,
            liquidity: 1_000_000_000_000u128,
            fee_bps: BasisPoints::new_const(30),
            tick_spacing: 60,
            initialized_ticks: vec![-120, -60, 0, 60, 120],
            tick_liquidity_net: HashMap::from([(60, -200_000i128), (120, -150_000i128)]),
            hook_mode: V4HookMode::NoHooks,
        },
        U256::from(100_000u64),
        SwapDirection::Token1ToToken0,
    )
}

fn fixture_curve() -> (CurvePoolSnapshot, usize, usize, U256) {
    (
        CurvePoolSnapshot {
            balances: vec![U256::from(1_000_000_000_000u64), U256::from(1_000_000_000_000u64)],
            decimals: vec![18, 18],
            stored_rates: None,
            variant: dex_math_core_rs::data::curve_registry::StableswapMathVariant::Vyper02ThreePool,
            amplification: U256::from(1000u64),
            fee_raw: U256::ZERO,
            fee_bps: 4,
        },
        0usize,
        1usize,
        U256::from(1_000_000u64),
    )
}

fn fixture_balancer() -> (BalancerPoolSnapshot, U256, SwapDirection) {
    (
        BalancerPoolSnapshot {
            balance0: U256::from(1_000_000_000u64),
            balance1: U256::from(2_000_000_000u64),
            weight0: w(50),
            weight1: w(50),
            swap_fee_bps: BasisPoints::new_const(30),
        },
        U256::from(10_000u64),
        SwapDirection::Token0ToToken1,
    )
}

fn print_results(title: &str, results: &[BenchResult]) {
    println!("=== {} ===", title);
    println!("case,iters,total_ns,ns_per_iter,ops_per_sec,checksum");
    for r in results {
        println!(
            "{},{},{},{:.2},{:.2},{}",
            r.name, r.iters, r.total_ns, r.ns_per_iter, r.ops_per_sec, r.checksum
        );
    }
}

fn main() {
    let base_iters = parse_iters();

    let (v3_pool, v3_in, v3_dir) = fixture_v3_cross();
    let (kyber_pool, kyber_in, kyber_dir) = fixture_kyber_cross();
    let (v4_pool, v4_in, v4_dir) = fixture_v4_cross();
    let (curve_pool, curve_in_idx, curve_out_idx, curve_in) = fixture_curve();
    let (bal_pool, bal_in, bal_dir) = fixture_balancer();

    let mut results = Vec::new();
    results.push(bench_case("v3_cross", base_iters, || {
        v3_quote_exact_input(&v3_pool, v3_in, v3_dir).unwrap().amount_out
    }));
    results.push(bench_case("kyber_cross", base_iters, || {
        kyber_quote_exact_input(&kyber_pool, kyber_in, kyber_dir).unwrap().amount_out
    }));
    results.push(bench_case("v4_cross", base_iters, || {
        v4_quote_exact_input(&v4_pool, v4_in, v4_dir).unwrap().amount_out
    }));
    results.push(bench_case("curve_basic", (base_iters / 5).max(10_000), || {
        curve_quote_exact_input(&curve_pool, curve_in_idx, curve_out_idx, curve_in)
            .unwrap()
            .amount_out
    }));
    results.push(bench_case("balancer_basic", base_iters, || {
        balancer_quote_exact_input(&bal_pool, bal_in, bal_dir).unwrap().amount_out
    }));

    print_results("perf_harness", &results);
}

