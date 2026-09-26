//! `builtins.toJSON` / `builtins.fromJSON`.

use hegel::TestCase;
use nix_pbt::*;

#[hegel::test]
fn from_json_to_json_roundtrip(tc: TestCase) {
    let v = tc.draw(nix_values());
    check(
        &format!("builtins.fromJSON (builtins.toJSON {})", v.to_nix()),
        Expect::Value(v.to_json()),
    );
}

/// Parsing JSON written by another serialiser (serde_json, compact or
/// pretty-printed) gives the value it describes.
#[hegel::test]
fn from_json(tc: TestCase) {
    let v = tc.draw(nix_values());
    let pretty = tc.draw(hegel::generators::booleans());
    let text = if pretty {
        serde_json::to_string_pretty(&v.to_json()).unwrap()
    } else {
        serde_json::to_string(&v.to_json()).unwrap()
    };
    check(
        &format!("builtins.fromJSON {}", nix_string_literal(&text)),
        Expect::Value(v.to_json()),
    );
}

/// `toJSON`'s output is valid JSON describing the value.
#[hegel::test]
fn to_json_parses(tc: TestCase) {
    let v = tc.draw(nix_values());
    let expr = format!("builtins.toJSON {}", v.to_nix());
    let outcomes = eval::eval_all(evaluators(), &expr);
    for (ev, out) in evaluators().iter().zip(outcomes) {
        let Outcome::Value(serde_json::Value::String(text)) = &out else {
            panic!("{}: {expr} gave {}", ev.name, out.describe());
        };
        let parsed: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("{}: {expr} gave invalid JSON {text:?}: {e}", ev.name));
        assert_eq!(parsed, v.to_json(), "{}: {expr} gave {text:?}", ev.name);
    }
}

/// The exact text `toJSON` produces (escapes, float formatting, key order)
/// is observable, e.g. through derivation hashes, so implementations should
/// agree on it byte for byte.
#[hegel::test]
fn to_json_text(tc: TestCase) {
    let v = tc.draw(nix_values());
    check(
        &format!("builtins.toJSON {}", v.to_nix()),
        Expect::Unspecified,
    );
}
