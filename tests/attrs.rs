//! Attribute set builtins.

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::*;
use serde_json::{Map, Value as Json};
use std::collections::BTreeMap;

type Attrs = BTreeMap<String, NixValue>;

fn attrs() -> impl hegel::generators::PrintableGenerator<Attrs> {
    gs::btree_maps(attr_names(), nix_scalars()).max_size(5)
}

fn attrs_nix(a: &Attrs) -> String {
    NixValue::Attrs(a.clone()).to_nix()
}

fn attrs_json(a: &Attrs) -> Json {
    NixValue::Attrs(a.clone()).to_json()
}

fn strings_nix(xs: &[String]) -> String {
    nix_list(xs.iter().map(|s| nix_string_literal(s)))
}

#[hegel::test]
fn attr_names_are_sorted(tc: TestCase) {
    let a = tc.draw(attrs());
    let names: Vec<&String> = a.keys().collect();
    check(
        &format!("builtins.attrNames {}", attrs_nix(&a)),
        Expect::value(serde_json::json!(names)),
    );
}

#[hegel::test]
fn attr_values(tc: TestCase) {
    let a = tc.draw(attrs());
    let values: Vec<Json> = a.values().map(NixValue::to_json).collect();
    check(
        &format!("builtins.attrValues {}", attrs_nix(&a)),
        Expect::value(values),
    );
}

#[hegel::test]
fn has_and_get_attr(tc: TestCase) {
    let a = tc.draw(attrs());
    let k = tc.draw(hegel::one_of!(
        attr_names(),
        gs::sampled_from(
            a.keys()
                .cloned()
                .chain(["a".to_string()])
                .collect::<Vec<_>>()
        ),
    ));
    let l = attrs_nix(&a);
    let k_lit = nix_string_literal(&k);
    check(
        &format!("builtins.hasAttr {k_lit} {l}"),
        Expect::value(a.contains_key(&k)),
    );
    let expect = match a.get(&k) {
        Some(v) => Expect::Value(v.to_json()),
        None => Expect::Error,
    };
    check(&format!("builtins.getAttr {k_lit} {l}"), expect);
}

#[hegel::test]
fn remove_attrs(tc: TestCase) {
    let a = tc.draw(attrs());
    let remove = tc.draw(gs::vecs(attr_names()).max_size(4));
    let mut want = a.clone();
    for k in &remove {
        want.remove(k);
    }
    check(
        &format!(
            "builtins.removeAttrs {} {}",
            attrs_nix(&a),
            strings_nix(&remove)
        ),
        Expect::Value(attrs_json(&want)),
    );
}

#[hegel::test]
fn intersect_attrs(tc: TestCase) {
    let a = tc.draw(attrs());
    let b = tc.draw(attrs());
    let want: Attrs = b
        .iter()
        .filter(|(k, _)| a.contains_key(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    check(
        &format!(
            "builtins.intersectAttrs {} {}",
            attrs_nix(&a),
            attrs_nix(&b)
        ),
        Expect::Value(attrs_json(&want)),
    );
}

/// `//` is right-biased.
#[hegel::test]
fn update_operator(tc: TestCase) {
    let a = tc.draw(attrs());
    let b = tc.draw(attrs());
    let mut want = a.clone();
    want.extend(b.clone());
    check(
        &format!("{} // {}", attrs_nix(&a), attrs_nix(&b)),
        Expect::Value(attrs_json(&want)),
    );
}

/// `listToAttrs` keeps the *first* occurrence of a duplicated name.
#[hegel::test]
fn list_to_attrs(tc: TestCase) {
    let pairs = tc.draw(gs::vecs(gs::tuples!(attr_names(), nix_scalars())).max_size(6));
    let mut want = Map::new();
    for (k, v) in &pairs {
        want.entry(k.clone()).or_insert_with(|| v.to_json());
    }
    let list = nix_list(pairs.iter().map(|(k, v)| {
        format!(
            "{{ name = {}; value = {}; }}",
            nix_string_literal(k),
            v.to_nix()
        )
    }));
    check(
        &format!("builtins.listToAttrs {list}"),
        Expect::Value(Json::Object(want)),
    );
}

#[hegel::test]
fn map_attrs(tc: TestCase) {
    let a = tc.draw(attrs());
    let want: Map<String, Json> = a
        .keys()
        .map(|k| (k.clone(), Json::String(k.clone())))
        .collect();
    check(
        &format!("builtins.mapAttrs (name: value: name) {}", attrs_nix(&a)),
        Expect::Value(Json::Object(want)),
    );
}

#[hegel::test]
fn cat_attrs(tc: TestCase) {
    let k = tc.draw(attr_names());
    let sets = tc.draw(gs::vecs(attrs()).max_size(4));
    let want: Vec<Json> = sets
        .iter()
        .filter_map(|s| s.get(&k))
        .map(NixValue::to_json)
        .collect();
    check(
        &format!(
            "builtins.catAttrs {} {}",
            nix_string_literal(&k),
            nix_list(sets.iter().map(attrs_nix))
        ),
        Expect::value(want),
    );
}

/// Round trip through `attrNames`/`attrValues`/`listToAttrs`.
#[hegel::test]
fn attrs_roundtrip(tc: TestCase) {
    let a = tc.draw(attrs());
    check_true(&format!(
        "let a = {}; in builtins.listToAttrs (map (n: {{ name = n; value = a.${{n}}; }}) (builtins.attrNames a)) == a",
        attrs_nix(&a)
    ));
}
