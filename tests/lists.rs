//! List builtins.

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::*;

fn int_list(xs: &[i64]) -> String {
    nix_list(xs.iter().map(|&x| nix_int_literal(x)))
}

fn value_list(xs: &[NixValue]) -> String {
    nix_list(xs.iter().map(NixValue::to_nix))
}

fn json_list(xs: &[NixValue]) -> serde_json::Value {
    xs.iter().map(NixValue::to_json).collect()
}

#[hegel::test]
fn length(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_values()).max_size(6));
    check(
        &format!("builtins.length {}", value_list(&xs)),
        Expect::value(xs.len()),
    );
}

#[hegel::test]
fn elem_at(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_values()).max_size(5));
    let i = tc.draw(hegel::one_of!(
        gs::integers::<i64>().min_value(-2).max_value(6),
        nix_ints()
    ));
    let expect = match usize::try_from(i).ok().and_then(|i| xs.get(i)) {
        Some(v) => Expect::Value(v.to_json()),
        None => Expect::Error,
    };
    check(
        &format!("builtins.elemAt {} {}", value_list(&xs), nix_int_literal(i)),
        expect,
    );
}

#[hegel::test]
fn head_tail(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_scalars()).max_size(4));
    let l = value_list(&xs);
    let expect = match xs.split_first() {
        Some((h, t)) => Expect::value(serde_json::json!([h.to_json(), json_list(t)])),
        None => Expect::Error,
    };
    check(
        &format!("[ (builtins.head {l}) (builtins.tail {l}) ]"),
        expect,
    );
}

#[hegel::test]
fn sort_ints(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_ints()).max_size(12));
    let mut sorted = xs.clone();
    sorted.sort();
    check(
        &format!("builtins.sort builtins.lessThan {}", int_list(&xs)),
        Expect::value(sorted),
    );
}

/// `sort` is documented to be stable: elements comparing equal keep their
/// relative order.
#[hegel::test]
fn sort_is_stable(tc: TestCase) {
    let pairs = tc.draw(
        gs::vecs(gs::tuples!(
            gs::integers::<i64>().min_value(0).max_value(3),
            gs::integers::<i64>().min_value(0).max_value(100),
        ))
        .max_size(40),
    );
    let mut sorted = pairs.clone();
    sorted.sort_by_key(|p| p.0);
    let list = nix_list(
        pairs
            .iter()
            .map(|(k, v)| format!("{{ k = {k}; v = {v}; }}")),
    );
    check(
        &format!("map (p: p.v) (builtins.sort (a: b: a.k < b.k) {list})"),
        Expect::value(sorted.iter().map(|p| p.1).collect::<Vec<_>>()),
    );
}

/// Sorting strings compares them byte-wise.
#[hegel::test]
fn sort_strings(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_strings()).max_size(8));
    let mut sorted = xs.clone();
    sorted.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    check(
        &format!(
            "builtins.sort builtins.lessThan {}",
            nix_list(xs.iter().map(|s| nix_string_literal(s)))
        ),
        Expect::value(sorted),
    );
}

#[hegel::test]
fn gen_list(tc: TestCase) {
    let n = tc.draw(gs::integers::<i64>().min_value(-3).max_value(20));
    let m = tc.draw(nix_ints());
    let expect: Expect = if n < 0 {
        Expect::Error
    } else {
        (0..n)
            .map(|i| i.checked_mul(m).ok_or(()))
            .collect::<Result<Vec<_>, _>>()
            .into()
    };
    check(
        &format!(
            "builtins.genList (i: i * {}) {}",
            nix_int_literal(m),
            nix_int_literal(n)
        ),
        expect,
    );
}

#[hegel::test]
fn concat_lists(tc: TestCase) {
    let xss = tc.draw(gs::vecs(gs::vecs(nix_scalars()).max_size(3)).max_size(4));
    let flat: Vec<NixValue> = xss.iter().flatten().cloned().collect();
    check(
        &format!(
            "builtins.concatLists {}",
            nix_list(xss.iter().map(|xs| value_list(xs)))
        ),
        Expect::Value(json_list(&flat)),
    );
}

#[hegel::test]
fn filter(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_ints()).max_size(10));
    let t = tc.draw(nix_ints());
    let want: Vec<i64> = xs.iter().copied().filter(|&x| x < t).collect();
    check(
        &format!(
            "builtins.filter (x: x < {}) {}",
            nix_int_literal(t),
            int_list(&xs)
        ),
        Expect::value(want),
    );
}

/// `elem` uses `==`, so ints and floats compare numerically.
#[hegel::test]
fn elem(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_values()).max_size(5));
    let x = tc.draw(hegel::one_of!(
        nix_values(),
        gs::sampled_from(
            xs.clone()
                .into_iter()
                .chain([NixValue::Null])
                .collect::<Vec<_>>()
        ),
    ));
    check(
        &format!("builtins.elem {} {}", x.to_nix(), value_list(&xs)),
        Expect::value(xs.iter().any(|y| nix_eq(&x, y))),
    );
}

#[hegel::test]
fn foldl_sum(tc: TestCase) {
    let xs = tc.draw(gs::vecs(nix_ints()).max_size(6));
    let sum = xs
        .iter()
        .try_fold(0i64, |acc, &x| acc.checked_add(x).ok_or(()));
    check(
        &format!("builtins.foldl' builtins.add 0 {}", int_list(&xs)),
        sum,
    );
}

#[hegel::test]
fn equality_is_reflexive(tc: TestCase) {
    let v = tc.draw(nix_values());
    let e = v.to_nix();
    check_true(&format!("{e} == {e}"));
}

#[hegel::test]
fn equality_matches_model(tc: TestCase) {
    let a = tc.draw(nix_values());
    let b = tc.draw(hegel::one_of!(nix_values(), gs::just(a.clone())));
    check(
        &format!("{} == {}", a.to_nix(), b.to_nix()),
        Expect::value(nix_eq(&a, &b)),
    );
}
