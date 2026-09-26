//! Flake references: `builtins.parseFlakeRef` / `builtins.flakeRefToString`.
//!
//! Ports the cases of `src/libflake-tests/flakeref.cc` from the Nix repo,
//! and generalises them into properties.

use hegel::TestCase;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use nix_pbt::*;
use serde_json::{Map, Value as Json};

fn round_trip(url: &str) -> String {
    format!(
        "builtins.flakeRefToString (builtins.parseFlakeRef {})",
        nix_string_literal(url)
    )
}

/// The table-driven cases from flakeref.cc: input URL, and the canonical
/// URL (`FlakeRef::to_string`) or `None` if parsing must fail.
const UNIT_CASES: &[(&str, Option<&str>)] = &[
    // TEST(parseFlakeRef, path)
    ("/foo/bar", Some("path:/foo/bar")),
    (
        "/foo/bar?revCount=123&rev=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("path:/foo/bar?rev=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa&revCount=123"),
    ),
    ("/foo/bar?xyzzy=123", None),
    ("/foo/bar#bla", None),
    (
        "/foo bar/baz?dir=bla space",
        Some("path:/foo%20bar/baz?dir=bla%20space"),
    ),
    // TEST(parseFlakeRef, GitArchiveInput)
    ("github:foo/bar/branch%23", Some("github:foo/bar/branch%23")),
    (
        "github:foo/bar?ref=branch%23",
        Some("github:foo/bar/branch%23"),
    ),
    // InputFromURLTest
    ("flake:nixpkgs", Some("flake:nixpkgs")),
    ("flake:nixpkgs/branch", Some("flake:nixpkgs/branch")),
    ("nixpkgs/branch", Some("flake:nixpkgs/branch")),
    (
        "nixpkgs/branch/2aae6c35c94fcfb415dbe95f408b9ce91ee846ed",
        Some("flake:nixpkgs/branch/2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"),
    ),
    ("nixpkgs/branch////", Some("flake:nixpkgs/branch")),
    (
        "nixpkgs/branch///2aae6c35c94fcfb415dbe95f408b9ce91ee846ed///",
        Some("flake:nixpkgs/branch/2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"),
    ),
    (
        "git://somewhere/repo?ref=branch",
        Some("git://somewhere/repo?ref=branch"),
    ),
    (
        "git+https://somewhere.aaaaaaa/repo?ref=branch",
        Some("git+https://somewhere.aaaaaaa/repo?ref=branch"),
    ),
    ("flake:/nixpkgs///branch////", Some("flake:nixpkgs/branch")),
    (
        "github://////owner%42/////repo%41///branch%43////",
        Some("github:ownerB/repoA/branchC"),
    ),
    (
        "gitlab:/owner%252Fsubgroup/////repo%41///branch%43////",
        Some("gitlab:owner%252Fsubgroup/repoA/branchC"),
    ),
    (
        "github:nixos/nix/0000000000000000000000000000000000000000",
        Some("github:nixos/nix/0000000000000000000000000000000000000000"),
    ),
    (
        "github:nixos/nix?rev=0000000000000000000000000000000000000000",
        Some("github:nixos/nix/0000000000000000000000000000000000000000"),
    ),
    (
        "github:nixos/nix//master///something/",
        Some("github:nixos/nix/master%2Fsomething"),
    ),
    // TEST(to_string, doesntReencodeUrl)
    (
        "http://localhost:8181/test/+3d.tar.gz",
        Some("http://localhost:8181/test/%2B3d.tar.gz"),
    ),
    // TEST(parseFlakeRef, urlInterpretationErrorsAreNotMasked)
    ("github:foo/bar?xyzzy=1", None),
    // TEST(parseFlakeRef, malformedGithubUrlDoesNotCrash)
    (
        "github:nixos/nixpkgs/nixpkgs.git?ref=aead170c1a49253ebfa5027010dfd89a77b73ca4",
        None,
    ),
];

/// Attributes from the InputFromURLTest cases: `parseFlakeRef` results.
const UNIT_ATTRS: &[(&str, &str)] = &[
    ("flake:nixpkgs", r#"{"id":"nixpkgs","type":"indirect"}"#),
    (
        "nixpkgs/branch/2aae6c35c94fcfb415dbe95f408b9ce91ee846ed",
        r#"{"id":"nixpkgs","type":"indirect","ref":"branch","rev":"2aae6c35c94fcfb415dbe95f408b9ce91ee846ed"}"#,
    ),
    (
        "git+https://somewhere.aaaaaaa/repo?ref=branch",
        r#"{"type":"git","ref":"branch","url":"https://somewhere.aaaaaaa/repo"}"#,
    ),
    (
        "github://////owner%42/////repo%41///branch%43////",
        r#"{"type":"github","owner":"ownerB","repo":"repoA","ref":"branchC"}"#,
    ),
    (
        "gitlab:/owner%252Fsubgroup/////repo%41///branch%43////",
        r#"{"type":"gitlab","owner":"owner%2Fsubgroup","repo":"repoA","ref":"branchC"}"#,
    ),
    (
        "github:nixos/nix//master///something/",
        r#"{"type":"github","owner":"nixos","repo":"nix","ref":"master/something"}"#,
    ),
    (
        "/foo bar/baz?dir=bla space",
        r#"{"type":"path","path":"/foo bar/baz","dir":"bla space"}"#,
    ),
];

/// Runs every case and reports all failures at once.
#[test]
fn nix_unit_test_cases() {
    let mut failures = Vec::new();
    let mut run = |expr: String, expect: Expect| {
        if let Err(e) = std::panic::catch_unwind(|| check(&expr, expect)) {
            failures.push(
                e.downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "panic".into()),
            );
        }
    };
    for (url, want) in UNIT_CASES {
        let expect = match want {
            Some(s) => Expect::value(*s),
            None => Expect::Error,
        };
        run(round_trip(url), expect);
    }
    for (url, attrs) in UNIT_ATTRS {
        let attrs: Json = serde_json::from_str(attrs).unwrap();
        run(
            format!("builtins.parseFlakeRef {}", nix_string_literal(url)),
            Expect::Value(attrs),
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        UNIT_CASES.len() + UNIT_ATTRS.len(),
        failures.join("\n\n")
    );
}

// ---------------------------------------------------------------------------
// Generators

/// Characters allowed in GitHub owner/repo names.
fn gh_name() -> impl PrintableGenerator<String> {
    gs::from_regex(r"[a-zA-Z0-9][a-zA-Z0-9_.-]{0,8}")
        .fullmatch(true)
        .filter(|s: &String| !s.ends_with(".git") && s != "." && s != "..")
}

/// Git ref names, loosely following `git check-ref-format`.
fn git_ref() -> impl PrintableGenerator<String> {
    gs::from_regex(r"[a-zA-Z0-9_][a-zA-Z0-9_+-]{0,6}(/[a-zA-Z0-9_][a-zA-Z0-9_.+-]{0,6}){0,2}")
        .fullmatch(true)
        .filter(|s: &String| !s.contains("..") && !s.ends_with('.') && !s.ends_with(".lock"))
}

fn rev() -> impl PrintableGenerator<String> {
    gs::from_regex(r"[0-9a-f]{40}").fullmatch(true)
}

/// Subdirectories, including characters that need percent-encoding.
fn subdir() -> impl PrintableGenerator<String> {
    gs::from_regex(r"[a-z0-9 _%+#?&=é-]{1,6}(/[a-z0-9 _%+#?&=é-]{1,6}){0,2}").fullmatch(true)
}

/// Absolute paths, including characters that need percent-encoding.
fn abs_path() -> impl PrintableGenerator<String> {
    gs::from_regex(r"(/[a-zA-Z0-9 _.%+#?&=@:é-]{1,6}){1,3}")
        .fullmatch(true)
        .filter(|s: &String| !s.split('/').any(|c| c == "." || c == ".."))
}

fn obj(pairs: Vec<(&str, Option<String>)>) -> Json {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), Json::String(v));
        }
    }
    Json::Object(m)
}

/// Flake reference attribute sets that should survive
/// `parseFlakeRef (flakeRefToString attrs)` unchanged.
#[hegel::composite]
fn flake_ref_attrs(tc: &TestCase) -> Json {
    let kind = tc.draw(gs::sampled_from(vec![
        "github",
        "gitlab",
        "sourcehut",
        "git",
        "path",
        "indirect",
        "tarball",
    ]));
    let opt = |g: &dyn Fn() -> String| -> Option<String> {
        if tc.draw(gs::booleans()) {
            Some(g())
        } else {
            None
        }
    };
    let dir = opt(&|| tc.draw(subdir()));
    let (r, v) = match tc.draw(gs::integers::<u8>().max_value(2)) {
        0 => (None, None),
        1 => (Some(tc.draw(git_ref())), None),
        _ => (None, Some(tc.draw(rev()))),
    };
    match kind {
        "github" | "gitlab" | "sourcehut" => {
            let owner = if kind == "sourcehut" {
                format!("~{}", tc.draw(gh_name()))
            } else {
                tc.draw(gh_name())
            };
            obj(vec![
                ("type", Some(kind.into())),
                ("owner", Some(owner)),
                ("repo", Some(tc.draw(gh_name()))),
                ("ref", r),
                ("rev", v),
                ("dir", dir),
            ])
        }
        "git" => {
            let scheme = tc.draw(gs::sampled_from(vec!["https", "ssh", "file", "http"]));
            // file:// URLs can't have an authority.
            let host = if scheme == "file" {
                String::new()
            } else {
                tc.draw(gs::from_regex(r"[a-z]{1,6}\.[a-z]{2,3}").fullmatch(true))
            };
            let url = format!("{scheme}://{host}/{}", tc.draw(gh_name()));
            obj(vec![
                ("type", Some("git".into())),
                ("url", Some(url)),
                ("ref", r),
                ("rev", v),
                ("dir", dir),
            ])
        }
        "path" => obj(vec![
            ("type", Some("path".into())),
            ("path", Some(tc.draw(abs_path()))),
            ("dir", dir),
        ]),
        "indirect" => obj(vec![
            ("type", Some("indirect".into())),
            (
                "id",
                Some(tc.draw(gs::from_regex(r"[a-zA-Z][a-zA-Z0-9_-]{0,8}").fullmatch(true))),
            ),
            ("ref", r),
            ("rev", v),
            ("dir", dir),
        ]),
        _ => obj(vec![
            ("type", Some("tarball".into())),
            (
                "url",
                Some(format!(
                    "https://{}/{}.tar.gz",
                    tc.draw(gs::from_regex(r"[a-z]{1,6}\.[a-z]{2,3}").fullmatch(true)),
                    tc.draw(gh_name()),
                )),
            ),
            ("dir", dir),
        ]),
    }
}

fn attrs_nix(v: &Json) -> String {
    let m = v.as_object().unwrap();
    let fields: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("{k} = {};", nix_string_literal(v.as_str().unwrap())))
        .collect();
    format!("{{ {} }}", fields.join(" "))
}

/// Strings that look like flake references: a scheme-ish prefix, some
/// path segments, maybe a query and a fragment, over an alphabet heavy in
/// URL metacharacters.
#[hegel::composite]
fn url_ish(tc: &TestCase) -> String {
    let scheme = tc.draw(gs::sampled_from(vec![
        "",
        "github:",
        "gitlab:",
        "sourcehut:",
        "git+https://",
        "git+file://",
        "path:",
        "flake:",
        "https://",
        "tarball+https://",
        "file://",
        "/",
        "./",
        "sourcehut:~",
    ]));
    let seg = || tc.draw(gs::text().alphabet("ab09._-~%2F+ #?&=:@/é").max_size(6));
    let n = tc.draw(gs::integers::<usize>().max_value(4));
    let mut s = scheme.to_string();
    let segs: Vec<String> = (0..n).map(|_| seg()).collect();
    s.push_str(&segs.join("/"));
    if tc.draw(gs::booleans()) {
        let key = tc.draw(gs::sampled_from(vec![
            "ref",
            "rev",
            "dir",
            "narHash",
            "host",
            "shallow",
            "submodules",
            "revCount",
            "xyzzy",
        ]));
        s.push_str(&format!("?{key}={}", seg()));
    }
    if tc.draw(gs::weighted_booleans(0.2)) {
        s.push_str(&format!("#{}", seg()));
    }
    s
}

// ---------------------------------------------------------------------------
// Properties

/// `parseFlakeRef (flakeRefToString attrs) == attrs`.
#[hegel::test]
fn attrs_round_trip(tc: TestCase) {
    let attrs = tc.draw(flake_ref_attrs().print_as_debug());
    check(
        &format!(
            "builtins.parseFlakeRef (builtins.flakeRefToString {})",
            attrs_nix(&attrs)
        ),
        Expect::Value(attrs),
    );
}

/// The canonical form is a fixed point:
/// `toString (parse (toString (parse s))) == toString (parse s)`.
#[hegel::test]
fn canonical_form_is_a_fixed_point(tc: TestCase) {
    let url = tc.draw(url_ish());
    let expr = round_trip(&url);
    if let Outcome::Value(Json::String(canonical)) = check(&expr, Expect::Unspecified) {
        check(&round_trip(&canonical), Expect::value(canonical));
    }
}

/// Parsing arbitrary URL-ish strings: the attributes (or the failure) agree.
#[hegel::test]
fn parse_url_ish(tc: TestCase) {
    let url = tc.draw(url_ish());
    check(
        &format!("builtins.parseFlakeRef {}", nix_string_literal(&url)),
        Expect::Unspecified,
    );
}

/// `flakeRefToString` of a generated attribute set: the URL (or the failure)
/// agrees.
#[hegel::test]
fn to_string_differential(tc: TestCase) {
    let attrs = tc.draw(flake_ref_attrs().print_as_debug());
    check(
        &format!("builtins.flakeRefToString {}", attrs_nix(&attrs)),
        Expect::Unspecified,
    );
}
