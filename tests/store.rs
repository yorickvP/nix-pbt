//! Store paths and derivations: `toFile`, `placeholder`, `derivation`
//! (`drvPath`/`outPath`), string context, `parseDrvName`, `hashString`.
//!
//! The store path models live in `src/store.rs`; they're exact, so these
//! properties pin down every byte of `drvPath`/`outPath`, which any
//! evaluator must reproduce to share binary caches with Nix.

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use nix_pbt::store::{self, Derivation};
use nix_pbt::*;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// Store path names: mostly valid, sometimes invalid (bad characters,
/// leading dots, too long).
fn store_names() -> impl PrintableGenerator<String> {
    hegel::one_of!(
        gs::from_regex(r"[a-zA-Z0-9+._?=-]{1,12}").fullmatch(true),
        gs::from_regex(r"\.{1,2}(-[a-z]{0,3})?").fullmatch(true),
        gs::text().alphabet("ab.-_ /é").min_size(1).max_size(6),
        gs::integers::<usize>()
            .min_value(205)
            .max_value(213)
            .map(|n| "n".repeat(n)),
    )
}

#[hegel::test]
fn to_file(tc: TestCase) {
    let name = tc.draw(store_names());
    let contents = tc.draw(nix_strings());
    let expect: Expect = match store::check_name(&name) {
        Ok(()) => Expect::value(store::text_path(&name, contents.as_bytes(), &[])),
        Err(_) => Expect::Error,
    };
    check(
        &format!(
            "builtins.toFile {} {}",
            nix_string_literal(&name),
            nix_string_literal(&contents)
        ),
        expect,
    );
}

#[hegel::test]
fn placeholder(tc: TestCase) {
    let output = tc.draw(nix_strings());
    check(
        &format!("builtins.placeholder {}", nix_string_literal(&output)),
        Expect::value(store::placeholder(&output)),
    );
}

// ---------------------------------------------------------------------------
// derivation

/// Values of derivation attributes, and how `coerceToString` (with
/// `coerceMore`) renders them into the environment. `None` for `null`.
#[derive(Clone, Debug)]
enum Attr {
    Str(String),
    Int(i64),
    Bool(bool),
    Null,
    List(Vec<Attr>),
}

hegel::pretty_print_as_debug!(Attr);

impl Attr {
    fn to_nix(&self) -> String {
        match self {
            Attr::Str(s) => nix_string_literal(s),
            Attr::Int(i) => nix_int_literal(*i),
            Attr::Bool(b) => b.to_string(),
            Attr::Null => "null".into(),
            Attr::List(xs) => nix_list(xs.iter().map(Attr::to_nix)),
        }
    }

    /// `coerceToString(..., coerceMore = true)`. List elements are
    /// separated by spaces, except after an element that is an empty list.
    fn coerce(&self) -> String {
        match self {
            Attr::Str(s) => s.clone(),
            Attr::Int(i) => i.to_string(),
            Attr::Bool(true) => "1".into(),
            Attr::Bool(false) | Attr::Null => String::new(),
            Attr::List(xs) => {
                let mut out = String::new();
                for (n, x) in xs.iter().enumerate() {
                    out.push_str(&x.coerce());
                    let empty_list = matches!(x, Attr::List(l) if l.is_empty());
                    if n + 1 < xs.len() && !empty_list {
                        out.push(' ');
                    }
                }
                out
            }
        }
    }
}

fn attrs() -> impl PrintableGenerator<Attr> {
    let scalar = || {
        hegel::one_of!(
            nix_strings().map(Attr::Str),
            nix_ints().map(Attr::Int),
            gs::booleans().map(Attr::Bool),
            gs::just(Attr::Null),
        )
    };
    gs::recursive(scalar(), |sub| gs::vecs(sub).max_size(3).map(Attr::List)).max_depth(3)
}

#[derive(Clone, Debug)]
struct DrvSpec {
    name: String,
    system: String,
    builder: String,
    args: Vec<Attr>,
    /// `None`: no `outputs` attribute (just "out").
    outputs: Option<Vec<String>>,
    ignore_nulls: bool,
    env: BTreeMap<String, Attr>,
}

hegel::pretty_print_as_debug!(DrvSpec);

#[hegel::composite]
fn drv_specs(tc: &TestCase) -> DrvSpec {
    DrvSpec {
        name: tc.draw(hegel::one_of!(
            gs::from_regex(r"[a-z][a-z0-9-]{0,8}").fullmatch(true),
            store_names()
        )),
        system: tc.draw(hegel::one_of!(
            gs::sampled_from(vec![
                "x86_64-linux".to_string(),
                "aarch64-darwin".to_string()
            ]),
            nix_strings()
        )),
        builder: tc.draw(hegel::one_of!(
            gs::just("/bin/sh".to_string()),
            nix_strings()
        )),
        args: tc.draw(gs::vecs(attrs()).max_size(3)),
        outputs: tc.draw(gs::optional(
            gs::vecs(gs::sampled_from(vec![
                "out", "dev", "lib", "bin", "doc", "drv", "a b", "é",
            ]))
            .max_size(3)
            .map(|v| v.into_iter().map(String::from).collect::<Vec<_>>()),
        )),
        ignore_nulls: tc.draw(gs::weighted_booleans(0.2)),
        env: tc.draw(
            gs::btree_maps(gs::from_regex(r"e[a-z0-9_]{0,3}").fullmatch(true), attrs()).max_size(4),
        ),
    }
}

impl DrvSpec {
    fn to_nix(&self) -> String {
        let mut fields = vec![
            format!("name = {};", nix_string_literal(&self.name)),
            format!("system = {};", nix_string_literal(&self.system)),
            format!("builder = {};", nix_string_literal(&self.builder)),
            format!("args = {};", nix_list(self.args.iter().map(Attr::to_nix))),
        ];
        if let Some(outputs) = &self.outputs {
            fields.push(format!(
                "outputs = {};",
                nix_list(outputs.iter().map(|o| nix_string_literal(o)))
            ));
        }
        if self.ignore_nulls {
            fields.push("__ignoreNulls = true;".into());
        }
        for (k, v) in &self.env {
            fields.push(format!("{k} = {};", v.to_nix()));
        }
        format!("derivation {{ {} }}", fields.join(" "))
    }

    /// The derivation Nix builds from these attributes, or why it can't.
    fn model(&self) -> Result<Derivation, String> {
        // Empty strings count as missing.
        if self.system.is_empty() || self.builder.is_empty() {
            return Err("required attribute missing".into());
        }
        // The platform is written into the .drv unescaped, so Nix fails to
        // read back a derivation whose `system` contains `"` or `\`.
        if self.system.contains(['"', '\\']) {
            return Err("unparsable .drv (system contains a quote or backslash)".into());
        }
        let outputs = self.outputs.clone().unwrap_or_else(|| vec!["out".into()]);
        if outputs.is_empty() {
            return Err("no outputs".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for o in &outputs {
            if !seen.insert(o) {
                return Err(format!("duplicate output {o}"));
            }
            store::check_name(o)?;
        }
        store::check_name(&self.name)?;
        store::check_name(&format!("{}.drv", self.name))?;
        for o in &outputs {
            if o != "out" {
                store::check_name(&format!("{}-{o}", self.name))?;
            }
        }
        let mut env = BTreeMap::new();
        env.insert("name".to_string(), self.name.clone());
        env.insert("system".to_string(), self.system.clone());
        env.insert("builder".to_string(), self.builder.clone());
        if self.outputs.is_some() {
            env.insert("outputs".to_string(), outputs.join(" "));
        }
        for (k, v) in &self.env {
            if self.ignore_nulls && matches!(v, Attr::Null) {
                continue;
            }
            env.insert(k.clone(), v.coerce());
        }
        Ok(Derivation {
            name: self.name.clone(),
            outputs,
            platform: self.system.clone(),
            builder: self.builder.clone(),
            args: self.args.iter().map(Attr::coerce).collect(),
            env,
        })
    }
}

/// `drvPath` and every output's path.
#[hegel::test]
fn derivation_paths(tc: TestCase) {
    let spec = tc.draw(drv_specs());
    let expect: Expect = match spec.model() {
        Ok(drv) => {
            let p = drv.paths();
            let outs: Vec<&String> = drv.outputs.iter().map(|o| &p.outputs[o]).collect();
            tc.note(&format!("model .drv:\n{}", drv.aterm(&p.outputs)));
            Expect::value(json!([p.drv_path, outs]))
        }
        Err(e) => {
            tc.note(&format!("model: error: {e}"));
            Expect::Error
        }
    };
    check(
        &format!(
            "let d = {}; in [ d.drvPath (map (o: d.${{o}}.outPath) (d.outputs or [ \"out\" ])) ]",
            spec.to_nix()
        ),
        expect,
    );
}

/// The string context of `drvPath` and of each output path.
#[hegel::test]
fn derivation_context(tc: TestCase) {
    let spec = tc.draw(drv_specs());
    let Ok(drv) = spec.model() else {
        tc.reject();
    };
    let p = drv.paths();
    let mut want = vec![json!({ p.drv_path.clone(): { "allOutputs": true } })];
    for o in &drv.outputs {
        want.push(json!({ p.drv_path.clone(): { "outputs": [o] } }));
    }
    check(
        &format!(
            "let d = {}; in map builtins.getContext ([ d.drvPath ] ++ map (o: d.${{o}}.outPath) (d.outputs or [ \"out\" ]))",
            spec.to_nix()
        ),
        Expect::value(want),
    );
}

// ---------------------------------------------------------------------------
// string context

/// A store object a string context can refer to. Nix only accepts context
/// for paths that exist, so expressions create these first (their paths
/// come from the store model).
#[derive(Clone, Debug)]
enum StoreObj {
    File(String, String),
    Drv(String, Vec<String>),
}

impl StoreObj {
    fn drv(name: &str, outputs: &[String]) -> Derivation {
        let mut env = BTreeMap::new();
        env.insert("name".to_string(), name.to_string());
        env.insert("system".to_string(), "x86_64-linux".to_string());
        env.insert("builder".to_string(), "/bin/sh".to_string());
        env.insert("outputs".to_string(), outputs.join(" "));
        Derivation {
            name: name.to_string(),
            outputs: outputs.to_vec(),
            platform: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec![],
            env,
        }
    }

    fn path(&self) -> String {
        match self {
            StoreObj::File(n, c) => store::text_path(n, c.as_bytes(), &[]),
            StoreObj::Drv(n, o) => Self::drv(n, o).paths().drv_path,
        }
    }

    /// An expression that creates the object and returns its path.
    fn to_nix(&self) -> String {
        match self {
            StoreObj::File(n, c) => format!(
                "(builtins.toFile {} {})",
                nix_string_literal(n),
                nix_string_literal(c)
            ),
            StoreObj::Drv(n, o) => format!(
                "(derivation {{ name = {}; system = \"x86_64-linux\"; builder = \"/bin/sh\"; outputs = {}; }}).drvPath",
                nix_string_literal(n),
                nix_list(o.iter().map(|x| nix_string_literal(x)))
            ),
        }
    }
}

/// Store objects, and a context (as `builtins.getContext` returns it)
/// referring to some of them.
#[derive(Clone, Debug)]
struct Ctx {
    objects: Vec<StoreObj>,
    json: Json,
}

hegel::pretty_print_as_debug!(Ctx);

#[hegel::composite]
fn contexts(tc: &TestCase) -> Ctx {
    let mut objects = Vec::new();
    let mut ctx = serde_json::Map::new();
    let n = tc.draw(gs::integers::<usize>().max_value(3));
    for _ in 0..n {
        let name = tc.draw(gs::from_regex(r"[a-z][a-z0-9-]{0,5}").fullmatch(true));
        let mut info = serde_json::Map::new();
        let obj = if tc.draw(gs::booleans()) {
            info.insert("path".into(), json!(true));
            StoreObj::File(name, tc.draw(nix_strings()))
        } else {
            let outs: std::collections::BTreeSet<String> = tc
                .draw(
                    gs::vecs(gs::sampled_from(vec!["out", "dev", "lib"]))
                        .min_size(1)
                        .max_size(3),
                )
                .into_iter()
                .map(String::from)
                .collect();
            let outs: Vec<String> = outs.into_iter().collect();
            if tc.draw(gs::booleans()) {
                info.insert("path".into(), json!(true));
            }
            if tc.draw(gs::booleans()) {
                info.insert("allOutputs".into(), json!(true));
            }
            let used: Vec<&String> = outs.iter().filter(|_| tc.draw(gs::booleans())).collect();
            if !used.is_empty() || info.is_empty() {
                let used: Vec<&String> = if used.is_empty() {
                    vec![&outs[0]]
                } else {
                    used
                };
                info.insert("outputs".into(), json!(used));
            }
            StoreObj::Drv(name, outs)
        };
        ctx.insert(obj.path(), Json::Object(info));
        objects.push(obj);
    }
    Ctx {
        objects,
        json: Json::Object(ctx),
    }
}

impl Ctx {
    /// The context as an attribute set for `appendContext`. The keys are
    /// plain strings (attribute names can't have context).
    fn to_nix(&self) -> String {
        let entries: Vec<String> = self
            .json
            .as_object()
            .unwrap()
            .iter()
            .map(|(path, info)| {
                let fields: Vec<String> = info
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| match v {
                        Json::Bool(b) => format!("{k} = {b};"),
                        Json::Array(xs) => format!(
                            "{k} = {};",
                            nix_list(xs.iter().map(|x| nix_string_literal(x.as_str().unwrap())))
                        ),
                        _ => unreachable!(),
                    })
                    .collect();
                format!("{} = {{ {} }};", nix_string_literal(path), fields.join(" "))
            })
            .collect();
        format!("{{ {} }}", entries.join(" "))
    }

    /// `body`, evaluated after the store objects have been created.
    fn around(&self, body: &str) -> String {
        if self.objects.is_empty() {
            return body.to_string();
        }
        format!(
            "builtins.seq (builtins.deepSeq {} null) ({body})",
            nix_list(self.objects.iter().map(StoreObj::to_nix))
        )
    }

    fn string(&self, s: &str) -> String {
        format!(
            "(builtins.appendContext {} {})",
            nix_string_literal(s),
            self.to_nix()
        )
    }
}

/// `getContext (appendContext s ctx) == ctx`: the NixStringContextElem
/// round trip from src/libexpr-tests/value/context.cc.
#[hegel::test]
fn append_context_round_trip(tc: TestCase) {
    let ctx = tc.draw(contexts());
    let s = tc.draw(nix_strings());
    check(
        &ctx.around(&format!("builtins.getContext {}", ctx.string(&s))),
        Expect::Value(ctx.json.clone()),
    );
}

/// Merge two contexts the way string concatenation does.
fn merge_contexts(a: &Json, b: &Json) -> Json {
    let mut out = a.as_object().unwrap().clone();
    for (path, info) in b.as_object().unwrap() {
        let entry = out.entry(path.clone()).or_insert_with(|| json!({}));
        let e = entry.as_object_mut().unwrap();
        for (k, v) in info.as_object().unwrap() {
            if k == "outputs" {
                let mut outs: std::collections::BTreeSet<String> = e
                    .get("outputs")
                    .and_then(Json::as_array)
                    .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
                    .unwrap_or_default();
                outs.extend(
                    v.as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string()),
                );
                e.insert("outputs".into(), json!(outs));
            } else {
                e.insert(k.clone(), v.clone());
            }
        }
    }
    Json::Object(out)
}

/// Context propagates through `+`, interpolation and `concatStringsSep`,
/// and is dropped by `unsafeDiscardStringContext`.
#[hegel::test]
fn context_propagation(tc: TestCase) {
    let (c1, c2) = (tc.draw(contexts()), tc.draw(contexts()));
    let (s1, s2) = (tc.draw(nix_strings()), tc.draw(nix_strings()));
    let (a, b) = (c1.string(&s1), c2.string(&s2));
    let merged = merge_contexts(&c1.json, &c2.json);
    let has = !merged.as_object().unwrap().is_empty();
    let body = format!(
        "[ (builtins.getContext ({a} + {b})) (builtins.getContext \"${{{a}}}${{{b}}}\") \
         (builtins.getContext (builtins.concatStringsSep \"\" [ {a} {b} ])) \
         (builtins.hasContext ({a} + {b})) \
         (builtins.hasContext (builtins.unsafeDiscardStringContext ({a} + {b}))) ]"
    );
    check(
        &c1.around(&c2.around(&body)),
        Expect::value(json!([merged, merged, merged, has, false])),
    );
}

/// Nix only accepts context that refers to existing store paths; this
/// asks for context on (random, so almost certainly absent) paths.
#[hegel::test]
fn append_context_nonexistent_path(tc: TestCase) {
    let hash = tc.draw(gs::from_regex(r"[0-9a-df-np-sv-z]{32}").fullmatch(true));
    let is_drv = tc.draw(gs::booleans());
    let path = format!(
        "{}/{hash}-x{}",
        store::STORE_DIR,
        if is_drv { ".drv" } else { "" }
    );
    let info = tc.draw(gs::sampled_from(vec![
        "path = true;",
        "allOutputs = true;",
        "outputs = [ \"out\" ];",
    ]));
    check(
        &format!(
            "builtins.getContext (builtins.appendContext \"\" {{ {} = {{ {info} }}; }})",
            nix_string_literal(&path)
        ),
        Expect::Error,
    );
}

/// How context survives other string builtins. Differential only: e.g.
/// whether `substring 0 0` keeps its argument's context isn't obvious.
#[hegel::test]
fn context_through_builtins(tc: TestCase) {
    let ctx = tc.draw(contexts());
    let s = tc.draw(nix_strings());
    let a = ctx.string(&s);
    let op = tc.draw(gs::sampled_from(vec![
        format!("builtins.substring 0 0 {a}"),
        format!("builtins.substring 0 1 {a}"),
        format!("builtins.replaceStrings [ \"a\" ] [ {a} ] \"xyz\""),
        format!("builtins.replaceStrings [ \"x\" ] [ {a} ] \"xyz\""),
        format!("builtins.replaceStrings [ \"x\" ] [ \"y\" ] {a}"),
        format!("builtins.toJSON [ {a} ]"),
        format!("builtins.head (builtins.split \"b\" {a})"),
        format!("builtins.head (builtins.match \"(.*)\" {a})"),
        format!("builtins.toString [ {a} ]"),
        format!("builtins.baseNameOf {a}"),
        format!("builtins.dirOf {a}"),
        format!("builtins.concatStringsSep {a} [ ]"),
        format!("builtins.concatStringsSep {a} [ \"x\" ]"),
        format!("builtins.concatStringsSep {a} [ \"x\" \"y\" ]"),
    ]));
    check(
        &ctx.around(&format!("builtins.getContext ({op})")),
        Expect::Unspecified,
    );
}

// ---------------------------------------------------------------------------
// parseDrvName, hashString

/// `DrvName::DrvName`: the version starts at the first dash that is followed
/// by something other than a letter.
fn parse_drv_name(s: &str) -> (String, String) {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'-' && i + 1 < b.len() && !b[i + 1].is_ascii_alphabetic() {
            return (s[..i].to_string(), s[i + 1..].to_string());
        }
    }
    (s.to_string(), String::new())
}

#[hegel::test]
fn parse_drv_name_model(tc: TestCase) {
    let s = tc.draw(hegel::one_of!(
        gs::from_regex(r"[a-z]{1,4}(-[a-z0-9.]{0,4}){0,3}-?").fullmatch(true),
        nix_strings(),
    ));
    let (name, version) = parse_drv_name(&s);
    check(
        &format!("builtins.parseDrvName {}", nix_string_literal(&s)),
        Expect::value(json!({ "name": name, "version": version })),
    );
}

fn hash_hex(algo: &str, data: &[u8]) -> Option<String> {
    use sha2::Digest;
    Some(match algo {
        "md5" => store::hex(&md5::Md5::digest(data)),
        "sha1" => store::hex(&sha1::Sha1::digest(data)),
        "sha256" => store::hex(&sha2::Sha256::digest(data)),
        "sha512" => store::hex(&sha2::Sha512::digest(data)),
        _ => return None,
    })
}

#[hegel::test]
fn hash_string(tc: TestCase) {
    let algo = tc.draw(gs::sampled_from(vec![
        "md5", "sha1", "sha256", "sha512", "sha384", "blake3", "SHA256", "",
    ]));
    let s = tc.draw(nix_strings());
    let expect: Expect = match hash_hex(algo, s.as_bytes()) {
        Some(h) => Expect::value(h),
        // sha384 and blake3 are valid in some implementations and not
        // others; that's for the differential check to report.
        None if algo == "sha384" || algo == "blake3" => Expect::Unspecified,
        None => Expect::Error,
    };
    check(
        &format!(
            "builtins.hashString {} {}",
            nix_string_literal(algo),
            nix_string_literal(&s)
        ),
        expect,
    );
}

/// `convertHash` between the hash formats (differential: Lix lacks it).
#[hegel::test]
fn convert_hash(tc: TestCase) {
    // Lix and fix don't have `convertHash`.
    if is_skipped("convert-hash") {
        return;
    }
    let algo = tc.draw(gs::sampled_from(vec!["md5", "sha1", "sha256", "sha512"]));
    let s = tc.draw(nix_strings());
    let to = tc.draw(gs::sampled_from(vec![
        "base16", "nix32", "base32", "base64", "sri",
    ]));
    let with_algo = tc.draw(gs::booleans());
    let h = hash_hex(algo, s.as_bytes()).unwrap();
    let hash_attr = if with_algo {
        format!("hash = \"{h}\"; hashAlgo = \"{algo}\";")
    } else {
        format!("hash = \"{algo}:{h}\";")
    };
    check(
        &format!("builtins.convertHash {{ {hash_attr} toHashFormat = \"{to}\"; }}"),
        Expect::Unspecified,
    );
}

/// A store operation that fails doesn't break the ones after it, in the same
/// session. Each sequence runs in a new session of every evaluator.
#[test]
fn store_errors_dont_poison_the_session() {
    let id = std::process::id();
    let mut sequences: Vec<(&str, Vec<(String, Expect)>)> = Vec::new();
    // fix: after the daemon rejects a write, later writes fail with
    // `WriteFailed` or with the earlier write's error.
    if !is_skipped("store-write-desync") {
        let mut seq = vec![("builtins.toFile \"..-x\" \"\"".to_string(), Expect::Error)];
        for i in 0..4 {
            let (name, contents) = (format!("ok-{id}-{i}"), format!("{i}"));
            seq.push((
                format!("builtins.toFile \"{name}\" \"{contents}\""),
                Expect::value(store::text_path(&name, contents.as_bytes(), &[])),
            ));
        }
        sequences.push(("store-write-desync", seq));
    }
    // Lix main: after copying a missing path fails, the next copy segfaults.
    if !is_skipped("failed-copy-crash") {
        let file = format!("{}/servers/nix-capi/server.c", env!("CARGO_MANIFEST_DIR"));
        sequences.push((
            "failed-copy-crash",
            vec![
                ("\"${/nix-pbt-missing}\"".to_string(), Expect::Error),
                (
                    format!("builtins.hashFile \"sha256\" \"${{{file}}}\""),
                    Expect::value(sha256_hex(&std::fs::read(&file).unwrap())),
                ),
            ],
        ));
    }
    let mut failures = Vec::new();
    for ev in evaluators() {
        for (tag, seq) in &sequences {
            let exprs: Vec<&str> = seq.iter().map(|(e, _)| e.as_str()).collect();
            let outs = ev.eval_in_new_session(&exprs);
            let ok = seq
                .iter()
                .zip(&outs)
                .all(|((_, want), got)| match (want, got) {
                    (Expect::Error, Outcome::Error(_)) => true,
                    (Expect::Value(w), Outcome::Value(g)) => w == g,
                    _ => false,
                });
            if !ok {
                let steps: Vec<String> = exprs
                    .iter()
                    .zip(&outs)
                    .map(|(e, o)| format!("  {e}\n    => {}", o.describe()))
                    .collect();
                failures.push(format!("{} ({tag}):\n{}", ev.name, steps.join("\n")));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
