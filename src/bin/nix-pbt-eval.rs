//! Evaluate expressions with every configured evaluator and print what each
//! one says, e.g. to reproduce a failure reported by a property test.
//!
//!   nix-pbt-eval 'builtins.splitVersion "__"'
//!   nix-pbt-eval --bench 500 '1 + 1'     # evaluations per second, per evaluator

use nix_pbt::evaluators;
use std::time::Instant;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut bench = None;
    if args.first().map(String::as_str) == Some("--bench") {
        args.remove(0);
        bench = Some(args.remove(0).parse::<usize>().expect("--bench N"));
    }
    if args.is_empty() {
        eprintln!("usage: nix-pbt-eval [--bench N] EXPR...");
        std::process::exit(2);
    }

    for expr in &args {
        if let Some(n) = bench {
            for ev in evaluators() {
                ev.eval(expr); // warm up (spawn the session)
                let start = Instant::now();
                for _ in 0..n {
                    ev.eval(expr);
                }
                let secs = start.elapsed().as_secs_f64();
                println!(
                    "{:>10}: {:>8.0} evals/s ({:.3} ms each)",
                    ev.name,
                    n as f64 / secs,
                    secs * 1000.0 / n as f64
                );
            }
        } else {
            println!("{expr}");
            for ev in evaluators() {
                println!("  {:>10}: {}", ev.name, ev.eval(expr).describe());
            }
        }
    }
}
