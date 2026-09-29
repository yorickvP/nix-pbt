//! Property-based and differential testing of Nix builtins.
//!
//! Every property builds a Nix expression, evaluates it with each configured
//! evaluator (see [`eval`]), and checks that
//!
//! 1. no evaluator crashed or timed out,
//! 2. all evaluators agree with the first one (the *reference*), and
//! 3. the result matches the Rust model of the builtin, when there is one.
//!
//! Evaluation errors are compared only by *whether* an error happened, never
//! by message, since messages differ legitimately between implementations.

pub mod eval;
pub mod generators;
pub mod store;
pub mod value;

pub use eval::{Evaluator, Outcome, evaluators};
pub use generators::*;
pub use value::{
    NixValue, nix_eq, nix_float_literal, nix_int_literal, nix_list, nix_string_literal,
};

use serde_json::Value as Json;
use std::fmt::Write as _;

/// What the model says an expression should evaluate to.
#[derive(Debug, Clone)]
pub enum Expect {
    /// Evaluates to this value (compared via its JSON form).
    Value(Json),
    /// Throws an evaluation error.
    Error,
    /// No model for this input: only check that the evaluators agree.
    Unspecified,
}

impl Expect {
    pub fn value(v: impl Into<Json>) -> Self {
        Expect::Value(v.into())
    }
}

impl<T: Into<Json>, E> From<Result<T, E>> for Expect {
    fn from(r: Result<T, E>) -> Self {
        match r {
            Ok(v) => Expect::Value(v.into()),
            Err(_) => Expect::Error,
        }
    }
}

/// Evaluate `expr` on every evaluator and check the three properties above.
///
/// Panics (failing the Hegel test case) with a report listing each
/// evaluator's outcome if anything is off. Otherwise returns the reference
/// evaluator's outcome, for properties that build on it.
pub fn check(expr: &str, expect: impl Into<Expect>) -> Outcome {
    check_with(expr, expect.into(), eval::eval_all)
}

/// [`check`], with the evaluators running one after the other.
///
/// For fresh git repositories: while read-write fix adds a tree to the store
/// at the same time, a long-running CppNix session sometimes reads an empty
/// `flake.nix` from it ("syntax error, unexpected end of file"). Not
/// reproduced outside the harness yet.
pub fn check_one_at_a_time(expr: &str, expect: impl Into<Expect>) -> Outcome {
    check_with(expr, expect.into(), eval::eval_each)
}

fn check_with(expr: &str, expect: Expect, eval: fn(&[Evaluator], &str) -> Vec<Outcome>) -> Outcome {
    let evs = evaluators();
    let outcomes = eval(evs, expr);
    log(evs, expr, &outcomes);

    let mut problems = Vec::new();
    let reference = &outcomes[0];
    for (ev, out) in evs.iter().zip(&outcomes) {
        if let Outcome::Crash(_) = out {
            problems.push(format!("{} crashed or timed out", ev.name));
        } else if !out.agrees_with(reference) {
            problems.push(format!("{} disagrees with {}", ev.name, evs[0].name));
        }
        let model_ok = match (&expect, out) {
            (Expect::Unspecified, _) => true,
            (Expect::Error, Outcome::Error(_)) => true,
            (Expect::Value(want), Outcome::Value(got)) => want == got,
            _ => false,
        };
        if !model_ok {
            problems.push(format!("{} disagrees with the model", ev.name));
        }
    }

    if !problems.is_empty() {
        let mut report = String::new();
        let _ = writeln!(report, "\nexpression:\n  {expr}\n");
        let _ = writeln!(report, "model:\n  {}\n", describe_expect(&expect));
        for (ev, out) in evs.iter().zip(&outcomes) {
            let _ = writeln!(report, "{}:\n  {}\n", ev.name, out.describe());
        }
        let _ = write!(report, "problems:\n  {}", problems.join("\n  "));
        panic!("{report}");
    }
    outcomes.into_iter().next().unwrap()
}

/// A named command from a `name=command args...;...` list in the
/// environment variable `var`, like `NIX_PBT_EVALUATORS` but for tests that
/// drive a CLI directly (e.g. `NIX_PBT_FLAKE_LOCK`).
pub struct NamedCommand {
    pub name: String,
    pub argv: Vec<String>,
}

pub fn commands(var: &str) -> Vec<NamedCommand> {
    std::env::var(var)
        .unwrap_or_default()
        .split(';')
        .filter(|e| !e.trim().is_empty())
        .map(|entry| {
            let (name, cmd) = entry
                .split_once('=')
                .unwrap_or_else(|| panic!("{var} entry {entry:?} is not name=command"));
            NamedCommand {
                name: name.trim().to_string(),
                argv: shell_words::split(cmd).unwrap_or_else(|e| panic!("{var}: {e}")),
            }
        })
        .collect()
}

impl NamedCommand {
    /// Run in `dir`; returns whether it succeeded, and its stderr.
    pub fn run_in(&self, dir: &std::path::Path) -> (bool, String) {
        let out = std::process::Command::new(&self.argv[0])
            .args(&self.argv[1..])
            .current_dir(dir)
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("could not run {}: {e}", self.argv[0]));
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Skip this test case if `tag` is listed in `NIX_PBT_SKIP` (comma
/// separated). Tests call this for classes of inputs with a known
/// divergence, so a run can get past them and find new ones, e.g.
/// `NIX_PBT_SKIP=circular-import cargo test --test flakes`.
pub fn skip_known(tc: &hegel::TestCase, tag: &str) {
    if is_skipped(tag) {
        tc.assume(false);
    }
}

/// Whether `tag` is listed in `NIX_PBT_SKIP`, for generators that can
/// avoid a known divergence up front rather than rejecting test cases.
pub fn is_skipped(tag: &str) -> bool {
    let skip = std::env::var("NIX_PBT_SKIP").unwrap_or_default();
    skip.split(',').any(|t| t.trim() == tag)
}

/// Where tests create files (generated flakes, file trees): `NIX_PBT_TMPDIR`,
/// or the system temporary directory. nix-shell deletes its `TMPDIR` on
/// exit, so set this to keep the trees of failing tests around.
pub fn tmp_dir() -> std::path::PathBuf {
    std::env::var_os("NIX_PBT_TMPDIR")
        .map(Into::into)
        .unwrap_or_else(std::env::temp_dir)
}

/// Shorthand for a law expressed in Nix: `expr` must evaluate to `true`.
pub fn check_true(expr: &str) {
    check(expr, Expect::value(true));
}

/// Lowercase hex SHA-256, as `builtins.hashString "sha256"` returns it.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Append every evaluation to the file named by `NIX_PBT_LOG`, as JSON
/// lines `{"expr": ..., "<evaluator>": "<outcome>", ...}`. Useful to find
/// what made Hegel report a flaky test.
fn log(evs: &[Evaluator], expr: &str, outcomes: &[Outcome]) {
    static LOG: std::sync::OnceLock<Option<std::sync::Mutex<std::fs::File>>> =
        std::sync::OnceLock::new();
    let file = LOG.get_or_init(|| {
        let path = std::env::var_os("NIX_PBT_LOG")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("NIX_PBT_LOG: {e}"));
        Some(std::sync::Mutex::new(file))
    });
    if let Some(file) = file {
        let mut entry = serde_json::Map::new();
        entry.insert("expr".into(), expr.into());
        for (ev, out) in evs.iter().zip(outcomes) {
            entry.insert(ev.name.clone(), out.describe().into());
        }
        use std::io::Write as _;
        let _ = writeln!(file.lock().unwrap(), "{}", Json::Object(entry));
    }
}

fn describe_expect(e: &Expect) -> String {
    match e {
        Expect::Value(v) => v.to_string(),
        Expect::Error => "<evaluation error>".into(),
        Expect::Unspecified => "<unspecified, differential only>".into(),
    }
}
