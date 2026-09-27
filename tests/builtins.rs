//! Every builtin applied to arbitrary arguments, inside `tryEval`.
//!
//! CppNix's `tryEval` catches only `throw`, failed assertions and failed
//! `<path>` lookups; every other error (type errors, `abort`, missing
//! attributes, ...) propagates through it. Evaluating
//!
//! ```nix
//! tryEval (deepSeq e true)
//! ```
//!
//! therefore tells three outcomes apart: a value, a *catchable* error and an
//! uncatchable one. Plain differential checks only see whether an error
//! happened, so this is what compares error kinds.
//!
//! Arguments usually have the type the builtin expects (so evaluation gets
//! past the type checks and into callbacks), sometimes not, and now and then
//! contain a `throw`, a failed assertion, a missing `<path>`, `abort`, or an
//! uncatchable error in a lazy position.

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::*;

#[derive(Clone, Copy, Debug)]
enum Arg {
    Any,
    Bool,
    Int,
    Num,
    Str,
    Path,
    List,
    Attrs,
    /// A function of this many arguments.
    Fun(u8),
}

use Arg::*;

/// CppNix's builtins and their parameters, minus the ones that fetch
/// (`fetch*`, `getFlake`, `parseFlakeRef` with a registry lookup) or aren't
/// deterministic (`currentTime`, `nixVersion`, ...).
const BUILTINS: &[(&str, &[Arg])] = &[
    ("abort", &[Str]),
    ("add", &[Num, Num]),
    ("addDrvOutputDependencies", &[Str]),
    ("addErrorContext", &[Str, Any]),
    ("all", &[Fun(1), List]),
    ("any", &[Fun(1), List]),
    ("appendContext", &[Str, Attrs]),
    ("attrNames", &[Attrs]),
    ("attrValues", &[Attrs]),
    ("baseNameOf", &[Str]),
    ("bitAnd", &[Int, Int]),
    ("bitOr", &[Int, Int]),
    ("bitXor", &[Int, Int]),
    ("break", &[Any]),
    ("catAttrs", &[Str, List]),
    ("ceil", &[Num]),
    ("compareVersions", &[Str, Str]),
    ("concatLists", &[List]),
    ("concatMap", &[Fun(1), List]),
    ("concatStringsSep", &[Str, List]),
    ("deepSeq", &[Any, Any]),
    ("derivation", &[Attrs]),
    ("derivationStrict", &[Attrs]),
    ("dirOf", &[Str]),
    ("div", &[Num, Num]),
    ("elem", &[Any, List]),
    ("elemAt", &[List, Int]),
    ("filter", &[Fun(1), List]),
    ("filterSource", &[Fun(2), Path]),
    ("findFile", &[List, Str]),
    ("flakeRefToString", &[Attrs]),
    ("floor", &[Num]),
    ("foldl'", &[Fun(2), Any, List]),
    ("fromJSON", &[Str]),
    ("fromTOML", &[Str]),
    ("functionArgs", &[Fun(1)]),
    ("genList", &[Fun(1), Int]),
    ("genericClosure", &[Attrs]),
    ("getAttr", &[Str, Attrs]),
    ("getContext", &[Str]),
    ("getEnv", &[Str]),
    ("groupBy", &[Fun(1), List]),
    ("hasAttr", &[Str, Attrs]),
    ("hasContext", &[Str]),
    ("hashFile", &[Str, Path]),
    ("hashString", &[Str, Str]),
    ("head", &[List]),
    ("import", &[Path]),
    ("intersectAttrs", &[Attrs, Attrs]),
    ("isAttrs", &[Any]),
    ("isBool", &[Any]),
    ("isFloat", &[Any]),
    ("isFunction", &[Any]),
    ("isInt", &[Any]),
    ("isList", &[Any]),
    ("isNull", &[Any]),
    ("isPath", &[Any]),
    ("isString", &[Any]),
    ("length", &[List]),
    ("lessThan", &[Any, Any]),
    ("listToAttrs", &[List]),
    ("map", &[Fun(1), List]),
    ("mapAttrs", &[Fun(2), Attrs]),
    ("match", &[Str, Str]),
    ("mul", &[Num, Num]),
    ("parseDrvName", &[Str]),
    ("partition", &[Fun(1), List]),
    ("path", &[Attrs]),
    ("pathExists", &[Path]),
    ("placeholder", &[Str]),
    ("readDir", &[Path]),
    ("readFile", &[Path]),
    ("readFileType", &[Path]),
    ("removeAttrs", &[Attrs, List]),
    ("replaceStrings", &[List, List, Str]),
    ("scopedImport", &[Attrs, Path]),
    ("seq", &[Any, Any]),
    ("sort", &[Fun(2), List]),
    ("split", &[Str, Str]),
    ("splitVersion", &[Str]),
    ("storePath", &[Str]),
    ("stringLength", &[Str]),
    ("sub", &[Num, Num]),
    ("substring", &[Int, Int, Str]),
    ("tail", &[List]),
    ("throw", &[Str]),
    ("toFile", &[Str, Str]),
    ("toJSON", &[Any]),
    ("toPath", &[Str]),
    ("toString", &[Any]),
    ("toXML", &[Any]),
    ("trace", &[Any, Any]),
    ("traceVerbose", &[Any, Any]),
    ("tryEval", &[Any]),
    ("typeOf", &[Any]),
    ("unsafeDiscardOutputDependency", &[Str]),
    ("unsafeDiscardStringContext", &[Str]),
    ("unsafeGetAttrPos", &[Str, Attrs]),
    ("warn", &[Str, Any]),
    ("zipAttrsWith", &[Fun(2), List]),
];

/// Strings that mean something to some builtin. No absolute paths of real
/// directories: `filterSource`/`path` would copy them to the store.
const STRINGS: &[&str] = &[
    "",
    "a",
    "abc",
    "é",
    "a b",
    "sha256",
    "md5",
    "nix32",
    "sri",
    "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
    "1.2.3",
    "foo-1.0",
    "out",
    "a.b",
    ".",
    "(a)(b)?",
    "[",
    "{\"a\":1}",
    "[1,2]",
    "a = 1",
    "x86_64-linux",
    "github:a/b",
    "path:/nix-pbt-missing",
    "/nix-pbt-missing",
    "/nix/store/00000000000000000000000000000000-x",
    "${builtins.toFile \"c\" \"\"}",
];

const KEYS: &[&str] = &[
    "a",
    "b",
    "name",
    "value",
    "key",
    "type",
    "path",
    "hash",
    "outPath",
    "__toString",
    "__functor",
    "outputs",
    "system",
    "builder",
    "startSet",
    "operator",
    "toHashFormat",
    "hashAlgo",
    "prefix",
    "filter",
    "owner",
    "repo",
    "url",
];

const FUNS1: &[&str] = &[
    "(x: x)",
    "(x: true)",
    "(x: [ x ])",
    "(x: { key = 1; })",
    "(x: \"s\")",
    "(x: throw \"f\")",
    "({ a, ... }: a)",
    "builtins.isInt",
    "builtins.attrNames",
    "toString",
];

const FUNS2: &[&str] = &[
    "(a: b: a)",
    "(a: b: b)",
    "(a: b: true)",
    "(a: b: a < b)",
    "(a: b: throw \"f\")",
    "builtins.add",
    "builtins.lessThan",
    "(n: v: [ n ])",
    "(p: t: t == \"directory\")",
];

/// Errors that `tryEval` catches in CppNix, and ones it doesn't.
const CATCHABLE: &[&str] = &["(throw \"t\")", "(assert false; 0)", "<nix-pbt-missing>"];
const UNCATCHABLE: &[&str] = &["(abort \"a\")", "({ }).missing", "(1 + \"a\")"];

struct Gen<'a> {
    tc: &'a TestCase,
    /// The errors planted in arguments. One kind per test case: with both,
    /// the result would depend on which is forced first, and evaluation
    /// order is unspecified (CppNix evaluates `a // b` right to left, Lix
    /// and fix left to right).
    errors: &'static [&'static str],
    /// Only generate arguments of the expected type (`NIX_PBT_SKIP=error-order`
    /// with catchable errors), so that no type error can race a planted
    /// `throw`. Errors are then only planted in callbacks: builtins also
    /// validate argument values (`hashFile "" (throw "t")`) in different
    /// orders.
    well_typed: bool,
}

impl Gen<'_> {
    fn pick(&self, xs: &[&str]) -> String {
        self.tc
            .draw_silent(gs::sampled_from(xs.to_vec()))
            .to_string()
    }

    fn int(&self, lo: i64, hi: i64) -> i64 {
        self.tc
            .draw_silent(gs::integers::<i64>().min_value(lo).max_value(hi))
    }

    fn chance(&self, p: f64) -> bool {
        self.tc.draw_silent(gs::weighted_booleans(p))
    }

    fn new(tc: &TestCase) -> Gen<'_> {
        let catchable = tc.draw_silent(gs::weighted_booleans(0.8));
        Gen {
            tc,
            errors: if catchable { CATCHABLE } else { UNCATCHABLE },
            well_typed: catchable && is_skipped("error-order"),
        }
    }

    /// A value for a parameter of kind `arg`: usually of that kind.
    fn arg(&self, arg: Arg, depth: usize) -> String {
        // fix doesn't force the function argument of `map`, `all`, ... when
        // the list is empty.
        if matches!(arg, Fun(_)) && is_skipped("empty-list-laziness") {
            return self.fun(arg);
        }
        if !self.well_typed && self.chance(0.05) {
            return self.pick(self.errors);
        }
        let arg = if !self.well_typed && self.chance(0.15) {
            Any
        } else {
            arg
        };
        let arg = match arg {
            Any => {
                [Bool, Int, Num, Str, Path, List, Attrs, Fun(1), Fun(2)][self.int(0, 8) as usize]
            }
            a => a,
        };
        match arg {
            Any => unreachable!(),
            Bool => self.pick(&["true", "false"]),
            Int => nix_int_literal(self.int(-1, 3)),
            Num => {
                if self.chance(0.3) {
                    self.pick(&["0.5", "(-1.5)", "0.0"])
                } else {
                    nix_int_literal(self.int(-1, 3))
                }
            }
            Str => {
                if !self.well_typed && self.chance(0.1) {
                    self.pick(&["null", "true", "false"])
                } else {
                    nix_string_or_interpolation(&self.pick(STRINGS))
                }
            }
            Path => self.pick(&[
                "./servers/nix-capi",
                "./servers/nix-capi/server.c",
                "./servers/nix-capi/package.nix",
                "/nix-pbt-missing",
            ]),
            List => {
                let n = if depth == 0 { 0 } else { self.int(0, 3) };
                let elem = if self.chance(0.5) {
                    Any
                } else {
                    [Int, Str, List, Attrs][self.int(0, 3) as usize]
                };
                let items: Vec<String> = (0..n)
                    .map(|_| self.arg(elem, depth.saturating_sub(1)))
                    .collect();
                nix_list(items)
            }
            Attrs => {
                let n = if depth == 0 { 0 } else { self.int(0, 3) };
                let mut fields = Vec::new();
                let mut seen = Vec::new();
                for _ in 0..n {
                    let k = self.pick(KEYS);
                    if seen.contains(&k) {
                        continue;
                    }
                    let v = self.arg(Any, depth.saturating_sub(1));
                    fields.push(format!("{} = {v};", nix_string_literal(&k)));
                    seen.push(k);
                }
                format!("{{ {} }}", fields.join(" "))
            }
            Fun(_) => self.fun(arg),
        }
    }

    fn fun(&self, arg: Arg) -> String {
        let (right, wrong) = match arg {
            Fun(1) => (FUNS1, FUNS2),
            _ => (FUNS2, FUNS1),
        };
        let f = self.pick(if self.chance(0.9) { right } else { wrong });
        // A throwing callback and a planted uncatchable error: which one
        // wins depends on evaluation order.
        if f.contains("throw") && self.errors == UNCATCHABLE && is_skipped("error-order") {
            return "(x: true)".into();
        }
        f
    }
}

/// Known divergences in `call`: skip the test case if it hits one listed in
/// `NIX_PBT_SKIP`.
fn skip_known_calls(tc: &TestCase, call: &str) {
    let context = call.contains("builtins.toFile \"c\"") || call.contains("./servers");
    let compares = call.contains('<') || call.contains("lessThan") || call.contains("sort");
    // fix: comparing a string with context to one without is a type error.
    if context && compares {
        skip_known(tc, "context-compare");
    }
    // fix: `toJSON` of a set whose `outPath` isn't a string or path is a
    // type error; Nix serialises the `outPath`, whatever it is.
    if call.contains("toJSON") && call.contains("\"outPath\"") {
        skip_known(tc, "tojson-outpath");
    }
    // Regular expressions aren't compatible yet (fix: `{"a":1}` is a
    // literal, CppNix: invalid).
    if call.contains("builtins.match") || call.contains("builtins.split") {
        skip_known(tc, "regex");
    }
    // fix prints primops as `<function />` in XML, Nix as `<unevaluated />`.
    const PRIMOPS: &[&str] = &[
        "builtins.isInt",
        "builtins.attrNames",
        "toString",
        "builtins.add",
        "builtins.lessThan",
    ];
    if call.contains("toXML") && PRIMOPS.iter().any(|p| call.contains(p)) {
        skip_known(tc, "toxml-primop");
    }
    // CppNix: functions never compare equal, not even to themselves (except
    // inside lists and sets, which compare elements by pointer first). Lix
    // ≥ 2.94 and fix compare them by identity.
    let has_fun = FUNS1.iter().chain(FUNS2).any(|f| call.contains(f));
    let compares_eq = ["==", "!=", "builtins.elem ", "<", "lessThan", "sort"];
    if has_fun && compares_eq.iter().any(|c| call.contains(c)) {
        skip_known(tc, "function-equality");
    }
    // fix: `splitVersion`/`compareVersions` differ, see `versions.rs`.
    if call.contains("splitVersion") || call.contains("compareVersions") {
        skip_known(tc, "versions");
    }
    // fix: these accept strings with context (see `context_is_rejected`).
    const LAX: &[&str] = &[
        "getEnv",
        "derivation",
        "placeholder",
        "unsafeGetAttrPos",
        "removeAttrs",
        "listToAttrs",
        "catAttrs",
        "groupBy",
        "match",
        "split",
        "parseDrvName",
        "compareVersions",
        "splitVersion",
        "findFile",
    ];
    if context && LAX.iter().any(|b| call.contains(b)) {
        skip_known(tc, "no-context");
    }
}

/// A string literal, except that `${...}` strings from [`STRINGS`] stay
/// interpolations (to get strings with context).
fn nix_string_or_interpolation(s: &str) -> String {
    if s.starts_with("${") {
        format!("\"{s}\"")
    } else {
        nix_string_literal(s)
    }
}

/// `tryEval (deepSeq e true)`, and the value of `e` as JSON if that
/// succeeded: functions, paths and attribute sets are mapped to JSON that
/// doesn't depend on `outPath`/`__toString` or on how paths are printed.
fn try_deep(e: &str) -> String {
    format!(
        "let safe = v: if builtins.isFunction v then \"<function>\" \
         else if builtins.isAttrs v then {{ attrs = map (n: [ n (safe v.${{n}}) ]) (builtins.attrNames v); }} \
         else if builtins.isList v then map safe v \
         else if builtins.isPath v then {{ path = toString v; }} \
         else v; \
         e = {e}; r = builtins.tryEval (builtins.deepSeq e true); \
         in if r.success then {{ value = safe e; }} else \"<caught>\""
    )
}

fn describe(out: &Outcome) -> &'static str {
    match out {
        Outcome::Value(serde_json::Value::String(s)) if s == "<caught>" => "caught",
        Outcome::Value(_) => "value",
        Outcome::Error(_) => "uncatchable error",
        _ => "other",
    }
}

/// fix doesn't check argument types of these as Nix does (it accepts paths,
/// sets and invalid context keys, or rejects sets with `outPath`).
const LAX_TYPES: &[&str] = &["appendContext", "storePath", "removeAttrs"];
const LAX_TYPES_WELL_TYPED: &[&str] = &[
    "hasContext",
    "toFile",
    "toPath",
    "unsafeDiscardOutputDependency",
    "sort",
    "filterSource",
];

#[hegel::test]
fn builtins_on_arbitrary_arguments(tc: TestCase) {
    let (name, params) = tc.draw_silent(gs::sampled_from(BUILTINS.to_vec()));
    if LAX_TYPES.contains(&name) {
        skip_known(&tc, "lax-types");
    }
    let mut g = Gen::new(&tc);
    if LAX_TYPES_WELL_TYPED.contains(&name) && is_skipped("lax-types") {
        g.well_typed = true;
    }
    let depth = tc.draw_silent(gs::integers::<usize>().max_value(2));
    // Now and then one argument too few (a partial application) or too many.
    let n = match tc.draw_silent(gs::integers::<u8>().max_value(19)) {
        0 => params.len().saturating_sub(1),
        1 => params.len() + 1,
        _ => params.len(),
    };
    let args: Vec<String> = (0..n)
        .map(|i| g.arg(params.get(i).copied().unwrap_or(Any), depth))
        .collect();
    let call = format!("builtins.{name} {}", args.join(" "));
    // fix: `genericClosure` requires `operator` even with nothing to apply
    // it to.
    if name == "genericClosure" && !call.contains("\"operator\"") {
        skip_known(&tc, "generic-closure-operator");
    }
    // fix: `foldl'` of an empty list returns the initial value unforced, and
    // applying that fails (see `foldl_initial_value_is_forced`).
    if name == "foldl'" && n > params.len() {
        skip_known(&tc, "foldl-thunk");
    }
    // fix: `throw` of anything but a string is a type error (see
    // `throw_coerces_its_argument`).
    if name == "throw" && !args.first().is_some_and(|a| a.starts_with('"')) {
        skip_known(&tc, "throw-coercion");
    }
    skip_known_calls(&tc, &call);
    let expr = try_deep(&call);
    tc.note(&format!("call: {call}"));
    let out = check(&expr, Expect::Unspecified);
    tc.event(describe(&out));
}

/// Operators and language constructs, the same way.
const OPERATORS: &[(&str, Arg, Arg)] = &[
    ("⟨a⟩ + ⟨b⟩", Num, Num),
    ("⟨a⟩ + ⟨b⟩", Str, Str),
    ("⟨a⟩ + ⟨b⟩", Path, Str),
    ("⟨a⟩ - ⟨b⟩", Num, Num),
    ("⟨a⟩ * ⟨b⟩", Num, Num),
    ("⟨a⟩ / ⟨b⟩", Num, Num),
    ("⟨a⟩ == ⟨b⟩", Any, Any),
    ("⟨a⟩ != ⟨b⟩", Any, Any),
    ("⟨a⟩ < ⟨b⟩", Num, Num),
    ("⟨a⟩ < ⟨b⟩", Str, Str),
    ("⟨a⟩ < ⟨b⟩", List, List),
    ("⟨a⟩ ++ ⟨b⟩", List, List),
    ("⟨a⟩ // ⟨b⟩", Attrs, Attrs),
    ("⟨a⟩ && ⟨b⟩", Bool, Bool),
    ("⟨a⟩ || ⟨b⟩", Bool, Bool),
    ("⟨a⟩ -> ⟨b⟩", Bool, Bool),
    ("!⟨a⟩", Bool, Any),
    ("-⟨a⟩", Num, Any),
    ("⟨a⟩ ⟨b⟩", Fun(1), Any),
    ("⟨a⟩.a", Attrs, Any),
    ("⟨a⟩.a or ⟨b⟩", Attrs, Any),
    ("⟨a⟩ ? a", Attrs, Any),
    ("\"${⟨a⟩}\"", Str, Any),
    ("if ⟨a⟩ then ⟨b⟩ else 0", Bool, Any),
    ("assert ⟨a⟩; ⟨b⟩", Bool, Any),
    ("with ⟨a⟩; a", Attrs, Any),
    ("let inherit (⟨a⟩) a; in a", Attrs, Any),
    ("({ a, ... }: a) ⟨a⟩", Attrs, Any),
    ("{ inherit (⟨a⟩) a; }", Attrs, Any),
    ("rec { a = ⟨a⟩; b = a; }", Any, Any),
    ("{ a = ⟨a⟩; } // ⟨b⟩", Any, Attrs),
    ("[ ⟨a⟩ ] ++ ⟨b⟩", Any, List),
];

#[hegel::test]
fn operators_on_arbitrary_operands(tc: TestCase) {
    let (template, ka, kb) = tc.draw_silent(gs::sampled_from(OPERATORS.to_vec()));
    let g = Gen::new(&tc);
    let (a, b) = (g.arg(ka, 1), g.arg(kb, 1));
    let e = template.replace("⟨a⟩", &a).replace("⟨b⟩", &b);
    // `0.a`: CppNix selects from `0`; Lix and fix reject it (Lix's
    // `tokens-no-whitespace` deprecation).
    if template.starts_with("⟨a⟩.a") && a.starts_with(|c: char| c.is_ascii_digit()) {
        skip_known(&tc, "number-select");
    }
    // `-./x` and `-/x` are path literals; fix reads them as negations.
    if e.starts_with("-.") || e.starts_with("-/") {
        skip_known(&tc, "path-lexing");
    }
    skip_known_calls(&tc, &e);
    let out = check(&try_deep(&e), Expect::Unspecified);
    tc.event(describe(&out));
}

/// `throw` coerces its argument to a string like string interpolation does,
/// so these are all ordinary, catchable `throw`s.
#[test]
fn throw_coerces_its_argument() {
    if is_skipped("throw-coercion") {
        return;
    }
    for arg in [
        "./servers",
        "{ __toString = s: \"m\"; }",
        "{ outPath = \"/x\"; }",
        "(derivation { name = \"d\"; system = \"x\"; builder = \"/b\"; })",
    ] {
        check(
            &format!("builtins.tryEval (builtins.throw {arg})"),
            Expect::value(serde_json::json!({ "success": false, "value": false })),
        );
    }
}

/// `foldl' op nul [ ]` is `nul`, which can be applied like any function.
#[test]
fn foldl_initial_value_is_forced() {
    if is_skipped("foldl-thunk") {
        return;
    }
    check(
        "builtins.foldl' (x: x) (let f = a: a; in f) [ ] 1",
        Expect::value(1),
    );
}

/// Every evaluator has the same builtins.
#[test]
fn builtin_names() {
    if is_skipped("convert-hash") {
        check(
            "builtins.attrNames (removeAttrs builtins [ \"convertHash\" ])",
            Expect::Unspecified,
        );
    } else {
        check("builtins.attrNames builtins", Expect::Unspecified);
    }
}

/// Places where CppNix (mostly) rejects strings with context
/// (`forceStringNoCtx`), with `C` where the string goes.
const NO_CONTEXT: &[&str] = &[
    "builtins.getEnv C",
    "(derivation { name = C; system = \"x\"; builder = \"/b\"; }).drvPath",
    "(derivation { name = \"a\"; system = C; builder = \"/b\"; }).drvPath",
    "(derivation { name = \"a\"; system = \"x\"; builder = \"/b\"; outputs = [ C ]; }).drvPath",
    "(derivation { name = \"a\"; system = \"x\"; builder = \"/b\"; outputHash = C; }).drvPath",
    "(derivation { name = \"a\"; system = \"x\"; builder = \"/b\"; outputHashAlgo = C; }).drvPath",
    "(derivation { name = \"a\"; system = \"x\"; builder = \"/b\"; outputHashMode = C; }).drvPath",
    "builtins.placeholder C",
    "builtins.findFile [ { prefix = C; path = ./servers; } ] \"x\"",
    "builtins.findFile [ ] C",
    "builtins.hashFile C ./servers/nix-capi/server.c",
    "builtins.fromJSON C",
    "builtins.fromTOML C",
    "builtins.toFile C \"\"",
    "builtins.path { name = C; path = ./servers/nix-capi; }",
    "builtins.path { sha256 = C; path = ./servers/nix-capi; }",
    "builtins.getAttr C { }",
    "builtins.hasAttr C { }",
    "builtins.unsafeGetAttrPos C { }",
    "builtins.removeAttrs { } [ C ]",
    "builtins.listToAttrs [ { name = C; value = 1; } ]",
    "builtins.catAttrs C [ ]",
    "builtins.groupBy (x: C) [ 1 ]",
    "builtins.hashString C \"\"",
    "builtins.match C \"\"",
    "builtins.split C \"\"",
    "builtins.parseDrvName C",
    "builtins.compareVersions C \"1\"",
    "builtins.compareVersions \"1\" C",
    "builtins.splitVersion C",
    "builtins.appendContext \"\" { \"${builtins.toFile \"d\" \"\"}\" = { outputs = [ C ]; }; }",
];

/// Strings with each kind of context, to put in `C`.
const WITH_CONTEXT: &[&str] = &[
    "\"${builtins.toFile \"c\" \"\"}\"",
    "\"${./servers/nix-capi/server.c}\"",
    "(derivation { name = \"d\"; system = \"x\"; builder = \"/b\"; }).drvPath",
    "(derivation { name = \"d\"; system = \"x\"; builder = \"/b\"; }).outPath",
    "builtins.substring 0 0 \"${builtins.toFile \"c\" \"\"}\"",
];

#[hegel::test]
fn context_is_rejected(tc: TestCase) {
    // fix accepts context in most of these.
    if is_skipped("no-context") {
        return;
    }
    let template = tc.draw(gs::sampled_from(NO_CONTEXT.to_vec()));
    let c = tc.draw(gs::sampled_from(WITH_CONTEXT.to_vec()));
    let e = template.replace('C', &format!("({c})"));
    // CppNix accepts context in `system` and `outputHashAlgo` (outside of
    // structured attrs), so this is differential only.
    check(&e, Expect::Unspecified);
}
