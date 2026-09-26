//! String builtins.

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::value::nix_list;
use nix_pbt::*;

/// Check a string-valued expression against a byte-level model. Strings that
/// aren't valid UTF-8 can't be printed as JSON (see
/// `json_output_of_invalid_utf8`), so for those we compare a hash instead.
fn check_bytes(expr: &str, want: Option<Vec<u8>>) -> Outcome {
    match want {
        None => check(expr, Expect::Error),
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(s) => check(expr, Expect::value(s)),
            Err(e) => check(
                &format!("builtins.hashString \"sha256\" ({expr})"),
                Expect::value(sha256_hex(e.as_bytes())),
            ),
        },
    }
}

#[hegel::test]
fn string_length(tc: TestCase) {
    let s = tc.draw(nix_strings());
    check(
        &format!("builtins.stringLength {}", nix_string_literal(&s)),
        Expect::value(s.len()),
    );
}

/// `substring start len s` works on bytes; a negative `len` means "to the
/// end", a negative `start` is an error.
fn substring_model(start: i64, len: i64, s: &[u8]) -> Option<Vec<u8>> {
    if start < 0 {
        return None;
    }
    let start = (start as u64).min(s.len() as u64) as usize;
    let end = if len < 0 {
        s.len()
    } else {
        start.saturating_add(len as usize).min(s.len())
    };
    Some(s[start..end].to_vec())
}

#[hegel::test]
fn substring(tc: TestCase) {
    let s = tc.draw(nix_strings());
    let start = tc.draw(nix_ints());
    let len = tc.draw(nix_ints());
    let expr = format!(
        "builtins.substring {} {} {}",
        nix_int_literal(start),
        nix_int_literal(len),
        nix_string_literal(&s)
    );
    check_bytes(&expr, substring_model(start, len, s.as_bytes()));
}

#[hegel::test]
fn concat_strings_sep(tc: TestCase) {
    let sep = tc.draw(nix_strings());
    let xs = tc.draw(gs::vecs(nix_strings()).max_size(5));
    let expr = format!(
        "builtins.concatStringsSep {} {}",
        nix_string_literal(&sep),
        nix_list(xs.iter().map(|x| nix_string_literal(x)))
    );
    check(&expr, Expect::value(xs.join(&sep)));
}

/// A transliteration of Nix's `replaceStrings`: scan left to right, at each
/// position try the patterns in order, and an empty pattern matches
/// everywhere (inserting its replacement and then copying one byte).
fn replace_strings_model(from: &[String], to: &[String], s: &[u8]) -> Option<Vec<u8>> {
    if from.len() != to.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut p = 0;
    while p <= s.len() {
        match from.iter().position(|f| s[p..].starts_with(f.as_bytes())) {
            Some(i) => {
                out.extend_from_slice(to[i].as_bytes());
                if from[i].is_empty() {
                    out.extend(s.get(p));
                    p += 1;
                } else {
                    p += from[i].len();
                }
            }
            None => {
                out.extend(s.get(p));
                p += 1;
            }
        }
    }
    Some(out)
}

#[hegel::test]
fn replace_strings(tc: TestCase) {
    let n = tc.draw(gs::integers::<usize>().max_value(3));
    let from = tc.draw(gs::vecs(nix_strings()).min_size(n).max_size(n));
    // Occasionally make the lists differ in length, which must be an error.
    let m = tc.draw(hegel::one_of!(
        gs::just(n),
        gs::integers::<usize>().max_value(3)
    ));
    let to = tc.draw(gs::vecs(nix_strings()).min_size(m).max_size(m));
    let s = tc.draw(nix_strings());
    let expr = format!(
        "builtins.replaceStrings {} {} {}",
        nix_list(from.iter().map(|x| nix_string_literal(x))),
        nix_list(to.iter().map(|x| nix_string_literal(x))),
        nix_string_literal(&s)
    );
    check_bytes(&expr, replace_strings_model(&from, &to, s.as_bytes()));
}

/// Printing a string that isn't valid UTF-8 as JSON should be an ordinary
/// evaluation error, not a crash.
#[hegel::test]
fn json_output_of_invalid_utf8(tc: TestCase) {
    let s = tc.draw(gs::text().min_codepoint(0x80).min_size(1).max_size(3));
    let cut = tc.draw(gs::integers::<usize>().min_value(1).max_value(s.len() - 1));
    tc.assume(!s.is_char_boundary(cut));
    check(
        &format!("builtins.substring 0 {cut} {}", nix_string_literal(&s)),
        Expect::Error,
    );
}

#[hegel::test]
fn to_string_int(tc: TestCase) {
    let i = tc.draw(nix_ints());
    check(
        &format!("builtins.toString {}", nix_int_literal(i)),
        Expect::value(i.to_string()),
    );
}

/// `toString` on floats has no simple model (it is `%g`-like and lossy), so
/// this is differential only.
#[hegel::test]
fn to_string_float(tc: TestCase) {
    let f = tc.draw(nix_floats());
    check(
        &format!("builtins.toString {}", nix_float_literal(f)),
        Expect::Unspecified,
    );
}

#[hegel::test]
fn string_escapes_roundtrip(tc: TestCase) {
    // Any string survives being written as a literal and read back.
    let s = tc.draw(gs::text().exclude_characters("\0").max_size(20));
    check(&nix_string_literal(&s), Expect::value(s));
}

#[hegel::test]
fn type_of(tc: TestCase) {
    let v = tc.draw(nix_values());
    check(
        &format!("builtins.typeOf {}", v.to_nix()),
        Expect::value(v.type_name()),
    );
}

#[hegel::test]
fn string_concat_is_associative(tc: TestCase) {
    let (a, b, c) = tc.draw(gs::tuples!(nix_strings(), nix_strings(), nix_strings()));
    let (a, b, c) = (
        nix_string_literal(&a),
        nix_string_literal(&b),
        nix_string_literal(&c),
    );
    check_true(&format!("({a} + {b}) + {c} == {a} + ({b} + {c})"));
}

#[hegel::test]
fn interpolation_is_concatenation(tc: TestCase) {
    let a = tc.draw(nix_strings());
    let b = tc.draw(nix_strings());
    let (la, lb) = (nix_string_literal(&a), nix_string_literal(&b));
    check_true(&format!(
        "let a = {la}; b = {lb}; in \"${{a}}${{b}}\" == a + b"
    ));
}
