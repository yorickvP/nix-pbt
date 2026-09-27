//! `builtins.splitVersion` / `builtins.compareVersions`.

use hegel::TestCase;
use hegel::generators::{self as gs, PrintableGenerator};
use nix_pbt::*;
use std::cmp::Ordering;

fn versions() -> impl PrintableGenerator<String> {
    hegel::one_of!(
        gs::from_regex(r"[0-9]{1,3}(\.([0-9]{1,3}|pre|rc|[a-c]{1,2}))*(-?(pre|rc)[0-9]?)?")
            .fullmatch(true),
        gs::text().alphabet("0129.-_aprec").max_size(10),
        gs::from_regex(r"[0-9]{18,22}(\.[0-9]{18,22})?").fullmatch(true),
        nix_strings(),
    )
}

fn is_sep(b: u8) -> bool {
    b == b'.' || b == b'-'
}

/// The next version component starting at `*p`: separators (`.` and `-`)
/// are skipped, then a run of digits or a run of non-digit non-separators.
fn next_component<'a>(s: &'a [u8], p: &mut usize) -> &'a [u8] {
    while *p < s.len() && is_sep(s[*p]) {
        *p += 1;
    }
    let start = *p;
    if *p < s.len() && s[*p].is_ascii_digit() {
        while *p < s.len() && s[*p].is_ascii_digit() {
            *p += 1;
        }
    } else {
        while *p < s.len() && !s[*p].is_ascii_digit() && !is_sep(s[*p]) {
            *p += 1;
        }
    }
    &s[start..*p]
}

fn split_version(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut p = 0;
    let mut out = Vec::new();
    while p < b.len() {
        let c = next_component(b, &mut p);
        if c.is_empty() {
            break;
        }
        out.push(String::from_utf8(c.to_vec()).unwrap());
    }
    out
}

/// Nix's component ordering: numbers compare numerically, the empty
/// component sorts before numbers, `pre` sorts before everything else, and
/// words sort before numbers (`2.3a < 2.3.1`).
///
/// Quirk: Nix parses components with `string2Int<int>`, so a digit run that
/// doesn't fit in 32 bits is treated as a *word*, which makes e.g.
/// `compareVersions "0" "2147483648"` return 1.
#[allow(clippy::if_same_then_else)] // mirrors the C++ branch by branch
fn component_lt(c1: &[u8], c2: &[u8]) -> bool {
    let num = |c: &[u8]| -> Option<i32> {
        std::str::from_utf8(c)
            .ok()
            .filter(|s| !s.is_empty())?
            .parse()
            .ok()
    };
    let (n1, n2) = (num(c1), num(c2));
    if let (Some(a), Some(b)) = (n1, n2) {
        a < b
    } else if c1.is_empty() && n2.is_some() {
        true
    } else if c1 == b"pre" && c2 != b"pre" {
        true
    } else if c2 == b"pre" {
        false
    } else if n2.is_some() {
        true
    } else if n1.is_some() {
        false
    } else {
        c1 < c2
    }
}

fn compare_versions(v1: &str, v2: &str) -> Ordering {
    let (s1, s2) = (v1.as_bytes(), v2.as_bytes());
    let (mut p1, mut p2) = (0, 0);
    while p1 < s1.len() || p2 < s2.len() {
        let c1 = next_component(s1, &mut p1);
        let c2 = next_component(s2, &mut p2);
        if component_lt(c1, c2) {
            return Ordering::Less;
        }
        if component_lt(c2, c1) {
            return Ordering::Greater;
        }
    }
    Ordering::Equal
}

/// fix: other characters than letters, digits, `.` and `-` are components
/// of their own (`"a_b"` gives `["a", "_", "b"]`), non-ASCII is a type
/// error, and components ≥ 2³¹ are numbers (in Nix, they don't fit an `int`
/// and count as words).
fn skip_fix_version_quirks(tc: &TestCase, v: &str) {
    let big_number = v.split(|c: char| !c.is_ascii_digit()).any(|n| {
        n.parse::<u64>()
            .map_or(!n.is_empty(), |n| n > i32::MAX as u64)
    });
    let odd = v
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'));
    if odd || big_number {
        skip_known(tc, "versions");
    }
}

#[hegel::test]
fn split_version_model(tc: TestCase) {
    let v = tc.draw(versions());
    skip_fix_version_quirks(&tc, &v);
    check(
        &format!("builtins.splitVersion {}", nix_string_literal(&v)),
        Expect::value(split_version(&v)),
    );
}

#[hegel::test]
fn compare_versions_model(tc: TestCase) {
    let a = tc.draw(versions());
    let b = tc.draw(versions());
    skip_fix_version_quirks(&tc, &a);
    skip_fix_version_quirks(&tc, &b);
    check(
        &format!(
            "builtins.compareVersions {} {}",
            nix_string_literal(&a),
            nix_string_literal(&b)
        ),
        Expect::value(compare_versions(&a, &b) as i8),
    );
}

#[hegel::test]
fn compare_versions_antisymmetric(tc: TestCase) {
    let a = nix_string_literal(&tc.draw(versions()));
    let b = nix_string_literal(&tc.draw(versions()));
    check_true(&format!(
        "builtins.compareVersions {a} {b} == - builtins.compareVersions {b} {a}"
    ));
}

#[hegel::test]
fn compare_versions_reflexive(tc: TestCase) {
    let a = nix_string_literal(&tc.draw(versions()));
    check(
        &format!("builtins.compareVersions {a} {a}"),
        Expect::value(0),
    );
}
