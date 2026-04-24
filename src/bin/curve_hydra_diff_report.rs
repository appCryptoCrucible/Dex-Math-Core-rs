use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Instant;

use alloy_primitives::U256;
use curve_math::Pool as CurveMathPool;
use dex_math_core_rs::data::curve_registry::StableswapMathVariant;
use dex_math_core_rs::dex::curve::adapter_math::{quote_exact_input, CurvePoolSnapshot};
use dex_math_core_rs::dex::curve::math;
use hydra_amm::config::HybridConfig;
use hydra_amm::domain::{
    Amount as HydraAmount, BasisPoints as HydraBasisPoints, Decimals as HydraDecimals,
    FeeTier as HydraFeeTier, SwapSpec as HydraSwapSpec, Token as HydraToken,
    TokenAddress as HydraTokenAddress, TokenPair as HydraTokenPair,
};
use hydra_amm::pools::HybridPool;
use hydra_amm::traits::{FromConfig, SwapPool};

#[derive(Clone, Copy)]
struct BenchStats {
    ns_per_iter: f64,
    ops_per_sec: f64,
}

fn bench<F>(iters: u64, mut f: F) -> BenchStats
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
    BenchStats {
        ns_per_iter,
        ops_per_sec: 1_000_000_000.0 / ns_per_iter,
    }
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

fn curve_fixture() -> CurvePoolSnapshot {
    CurvePoolSnapshot {
        balances: vec![U256::from(1_000_000_000_000u64), U256::from(1_000_000_000_000u64)],
        decimals: vec![18, 18],
        stored_rates: None,
        precomputed_rates: None,
        variant: StableswapMathVariant::Vyper02ThreePool,
        amplification: U256::from(1000u64),
        fee_raw: U256::ZERO,
        fee_bps: 4,
    }
}

fn curve_math_fixture_from_ours(pool: &CurvePoolSnapshot, amp_scale: u64) -> CurveMathPool {
    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    CurveMathPool::StableSwapV2 {
        balances: pool.balances.clone(),
        rates: vec![one_e18, one_e18],
        amp: pool.amplification * U256::from(amp_scale),
        fee: U256::from(4_000_000u64),
    }
}

fn hydra_fixture() -> (HybridPool, HydraToken) {
    let decimals = HydraDecimals::new(18).expect("decimals");
    let token_a = HydraToken::new(HydraTokenAddress::from_bytes([7u8; 32]), decimals);
    let token_b = HydraToken::new(HydraTokenAddress::from_bytes([8u8; 32]), decimals);
    let pair = HydraTokenPair::new(token_a, token_b).expect("pair");
    let fee = HydraFeeTier::new(HydraBasisPoints::new(4));
    let cfg = HybridConfig::new(
        pair,
        fee,
        1000,
        HydraAmount::new(1_000_000_000_000u128),
        HydraAmount::new(1_000_000_000_000u128),
    )
    .expect("hybrid cfg");
    let pool = HybridPool::from_config(&cfg).expect("hybrid pool");
    (pool, token_a)
}

fn rand_amount(seed: &mut u64, upper: u64) -> U256 {
    // xorshift64*
    let mut x = *seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    let v = x.wrapping_mul(0x2545F4914F6CDD1D) % upper.max(2);
    U256::from(v.max(1))
}

fn absolute_diff(a: U256, b: U256) -> U256 {
    if a >= b { a - b } else { b - a }
}

fn write_report(
    iters: u64,
    curve_total: BenchStats,
    curve_setup: BenchStats,
    curve_kernel: BenchStats,
    curve_enrichment: BenchStats,
    curve_math_enum: BenchStats,
    curve_math_kernel: BenchStats,
    hydra_total: BenchStats,
    hydra_clone: BenchStats,
    hydra_swap_only_est_ns: f64,
    curve_parity_exact: usize,
    curve_parity_total: usize,
    curve_max_diff: U256,
    hydra_parity_exact: usize,
    hydra_parity_total: usize,
    hydra_max_diff: U256,
) -> std::io::Result<PathBuf> {
    let mut md = String::new();
    writeln!(&mut md, "# Curve/Hydra Differential Report").ok();
    writeln!(&mut md).ok();
    writeln!(&mut md, "- Iterations: `{iters}`").ok();
    writeln!(&mut md, "- Unit: `ns/op`").ok();
    writeln!(
        &mut md,
        "- Scope: curve-family adapter path differential + parity checks."
    )
    .ok();
    writeln!(&mut md).ok();

    writeln!(&mut md, "## Timing Breakdown").ok();
    writeln!(&mut md).ok();
    writeln!(&mut md, "| Path | ns/op | ops/sec |").ok();
    writeln!(&mut md, "|---|---:|---:|").ok();
    writeln!(
        &mut md,
        "| ours_curve_total_quote | {:.2} | {:.2} |",
        curve_total.ns_per_iter, curve_total.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| ours_curve_setup_rates_xp | {:.2} | {:.2} |",
        curve_setup.ns_per_iter, curve_setup.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| ours_curve_kernel_dy | {:.2} | {:.2} |",
        curve_kernel.ns_per_iter, curve_kernel.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| ours_curve_enrichment_prices_impact | {:.2} | {:.2} |",
        curve_enrichment.ns_per_iter, curve_enrichment.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| curve_math_pool_get_amount_out | {:.2} | {:.2} |",
        curve_math_enum.ns_per_iter, curve_math_enum.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| curve_math_stableswap_v2_kernel | {:.2} | {:.2} |",
        curve_math_kernel.ns_per_iter, curve_math_kernel.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| hydra_hybrid_total_swap | {:.2} | {:.2} |",
        hydra_total.ns_per_iter, hydra_total.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| hydra_hybrid_clone_only | {:.2} | {:.2} |",
        hydra_clone.ns_per_iter, hydra_clone.ops_per_sec
    )
    .ok();
    writeln!(
        &mut md,
        "| hydra_hybrid_swap_core_estimate (total-clone) | {:.2} | {:.2} |",
        hydra_swap_only_est_ns,
        1_000_000_000.0 / hydra_swap_only_est_ns.max(1.0)
    )
    .ok();
    writeln!(&mut md).ok();

    writeln!(&mut md, "## Differential").ok();
    writeln!(&mut md).ok();
    let ours_vs_curve = curve_total.ns_per_iter / curve_math_enum.ns_per_iter;
    let ours_vs_hydra = curve_total.ns_per_iter / hydra_total.ns_per_iter;
    let curve_dispatch_overhead = if curve_math_enum.ns_per_iter > curve_math_kernel.ns_per_iter {
        ((curve_math_enum.ns_per_iter - curve_math_kernel.ns_per_iter) / curve_math_enum.ns_per_iter) * 100.0
    } else {
        0.0
    };
    writeln!(
        &mut md,
        "- Ours vs curve-math total: `{:.3}x` slower.",
        ours_vs_curve
    )
    .ok();
    writeln!(
        &mut md,
        "- Ours vs hydra-hybrid total: `{:.3}x` slower.",
        ours_vs_hydra
    )
    .ok();
    let enrichment_exclusive = (curve_enrichment.ns_per_iter - curve_kernel.ns_per_iter).max(0.0);
    writeln!(
        &mut md,
        "- Ours setup+enrichment(exclusive) share estimate: `{:.2}%` of total.",
        ((curve_setup.ns_per_iter + enrichment_exclusive) / curve_total.ns_per_iter) * 100.0
    )
    .ok();
    writeln!(
        &mut md,
        "- curve-math enum dispatch overhead estimate: `{:.2}%` (pool dispatch vs direct kernel).",
        curve_dispatch_overhead
    )
    .ok();
    writeln!(
        &mut md,
        "- hydra clone overhead estimate in harness: `{:.2}%` of measured total.",
        (hydra_clone.ns_per_iter / hydra_total.ns_per_iter) * 100.0
    )
    .ok();
    writeln!(&mut md).ok();

    writeln!(&mut md, "## Accuracy/Parity").ok();
    writeln!(&mut md).ok();
    writeln!(
        &mut md,
        "- Curve parity (ours vs curve-math StableSwapV2 mapping): exact matches `{}/{} ({:.2}%)`, max abs diff `{}` wei.",
        curve_parity_exact,
        curve_parity_total,
        (curve_parity_exact as f64 / curve_parity_total as f64) * 100.0,
        curve_max_diff
    )
    .ok();
    writeln!(
        &mut md,
        "- Hydra parity (ours curve adapter vs hydra hybrid): exact matches `{}/{} ({:.2}%)`, max abs diff `{}` wei.",
        hydra_parity_exact,
        hydra_parity_total,
        (hydra_parity_exact as f64 / hydra_parity_total as f64) * 100.0,
        hydra_max_diff
    )
    .ok();
    writeln!(&mut md).ok();
    writeln!(
        &mut md,
        "Note: parity numbers above are for this exact fixture and amp/fee mapping only; they are not universal proofs across all pool templates."
    )
    .ok();

    fs::create_dir_all("benches")?;
    let path = PathBuf::from("benches").join("curve_hydra_differential.md");
    fs::write(&path, md)?;
    Ok(path)
}

fn main() {
    let iters = parse_iters();
    let pool = curve_fixture();
    let amount_in = U256::from(1_000_000u64);

    let rates = math::stableswap_rates_resolve(&pool.decimals, None).expect("rates");
    let xp_before = math::stableswap_xp_from_rates(&pool.balances, &rates).expect("xp");

    let curve_math_pool = curve_math_fixture_from_ours(&pool, 100);

    let curve_total = bench((iters / 4).max(10_000), || {
        let out = quote_exact_input(&pool, 0, 1, amount_in).expect("ours total").amount_out;
        black_box(out);
        true
    });

    let curve_setup = bench((iters / 2).max(20_000), || {
        let rates = math::stableswap_rates_resolve(&pool.decimals, None).expect("rates");
        let xp = math::stableswap_xp_from_rates(&pool.balances, &rates).expect("xp");
        black_box((rates, xp));
        true
    });

    let curve_kernel = bench((iters / 2).max(20_000), || {
        let out = math::calculate_swap_output_from_xp(
            amount_in,
            0,
            1,
            &xp_before,
            &rates,
            pool.variant,
            pool.amplification,
            pool.fee_raw,
            pool.fee_bps,
        )
        .expect("kernel");
        black_box(out);
        true
    });

    let curve_enrichment = bench((iters / 2).max(20_000), || {
        let spot_before = math::calculate_curve_price(0, 1, &xp_before, &rates, pool.variant, pool.amplification)
            .expect("spot before");
        let amount_out = math::calculate_swap_output_from_xp(
            amount_in,
            0,
            1,
            &xp_before,
            &rates,
            pool.variant,
            pool.amplification,
            pool.fee_raw,
            pool.fee_bps,
        )
        .expect("out");
        let mut balances_after = pool.balances.clone();
        balances_after[0] += amount_in;
        balances_after[1] -= amount_out;
        let xp_after = math::stableswap_xp_from_rates(&balances_after, &rates).expect("xp after");
        let spot_after = math::calculate_curve_price(0, 1, &xp_after, &rates, pool.variant, pool.amplification)
            .expect("spot after");
        black_box((spot_before, spot_after, balances_after));
        true
    });

    let curve_math_enum = bench((iters / 4).max(10_000), || {
        let out = curve_math_pool
            .get_amount_out(0, 1, amount_in)
            .expect("curve-math pool");
        black_box(out);
        true
    });

    let one_e18 = U256::from(10u64).pow(U256::from(18u64));
    let curve_math_kernel = bench((iters / 4).max(10_000), || {
        let out = curve_math::swap::stableswap_v2::get_amount_out(
            &pool.balances,
            &[one_e18, one_e18],
            pool.amplification * U256::from(100u64),
            U256::from(4_000_000u64),
            0,
            1,
            amount_in,
        )
        .expect("curve kernel");
        black_box(out);
        true
    });

    let (hydra_pool, hydra_token_a) = hydra_fixture();
    let swap_spec = HydraSwapSpec::exact_in(HydraAmount::new(1_000_000u128)).expect("spec");

    let hydra_clone = bench((iters / 2).max(20_000), || {
        let c = hydra_pool.clone();
        black_box(c);
        true
    });

    let hydra_total = bench((iters / 4).max(10_000), || {
        let mut p = hydra_pool.clone();
        let out = p.swap(swap_spec, hydra_token_a).expect("hydra").amount_out();
        let _ = black_box(out);
        true
    });
    let hydra_swap_only_est_ns = (hydra_total.ns_per_iter - hydra_clone.ns_per_iter).max(1.0);

    // Parity sweep over randomized pool states.
    let parity_n = 1000usize;
    let mut seed = 0xC0FFEE_u64;
    let mut curve_exact = 0usize;
    let mut curve_max_diff = U256::ZERO;
    let mut hydra_exact = 0usize;
    let mut hydra_max_diff = U256::ZERO;

    for _ in 0..parity_n {
        let reserve0_u64 = rand_amount(&mut seed, 4_000_000_000_000u64).to::<u64>() + 1_000_000u64;
        let reserve1_u64 = rand_amount(&mut seed, 4_000_000_000_000u64).to::<u64>() + 1_000_000u64;
        let amp_u64 = (rand_amount(&mut seed, 3_500u64).to::<u64>() + 10).min(10_000);
        let max_in = (reserve0_u64 / 200).max(1_000);
        let amt = rand_amount(&mut seed, max_in);

        let parity_pool = CurvePoolSnapshot {
            balances: vec![U256::from(reserve0_u64), U256::from(reserve1_u64)],
            decimals: vec![18, 18],
            stored_rates: None,
            precomputed_rates: None,
            variant: StableswapMathVariant::Vyper02ThreePool,
            amplification: U256::from(amp_u64),
            fee_raw: U256::ZERO,
            fee_bps: 4,
        };
        let ours = quote_exact_input(&parity_pool, 0, 1, amt)
            .expect("ours parity")
            .amount_out;

        let cm_pool = curve_math_fixture_from_ours(&parity_pool, 100);
        let c = cm_pool.get_amount_out(0, 1, amt).expect("curve parity");
        if ours == c {
            curve_exact += 1;
        }
        let d = absolute_diff(ours, c);
        if d > curve_max_diff {
            curve_max_diff = d;
        }

        let decimals = HydraDecimals::new(18).expect("decimals parity");
        let token_a = HydraToken::new(HydraTokenAddress::from_bytes([11u8; 32]), decimals);
        let token_b = HydraToken::new(HydraTokenAddress::from_bytes([12u8; 32]), decimals);
        let pair = HydraTokenPair::new(token_a, token_b).expect("pair parity");
        let fee = HydraFeeTier::new(HydraBasisPoints::new(4));
        let cfg = HybridConfig::new(
            pair,
            fee,
            amp_u64 as u32,
            HydraAmount::new(reserve0_u64 as u128),
            HydraAmount::new(reserve1_u64 as u128),
        )
        .expect("cfg parity");
        let mut p = HybridPool::from_config(&cfg).expect("pool parity");
        let spec = HydraSwapSpec::exact_in(HydraAmount::new(amt.to::<u128>()))
            .expect("hydra spec parity");
        let h = U256::from(p.swap(spec, token_a).expect("hydra parity").amount_out().get());
        if ours == h {
            hydra_exact += 1;
        }
        let dh = absolute_diff(ours, h);
        if dh > hydra_max_diff {
            hydra_max_diff = dh;
        }
    }

    let report = write_report(
        iters,
        curve_total,
        curve_setup,
        curve_kernel,
        curve_enrichment,
        curve_math_enum,
        curve_math_kernel,
        hydra_total,
        hydra_clone,
        hydra_swap_only_est_ns,
        curve_exact,
        parity_n,
        curve_max_diff,
        hydra_exact,
        parity_n,
        hydra_max_diff,
    )
    .expect("write report");

    println!("Wrote differential report: {}", report.display());
}
