//! Arithmetic and comparison builtins.

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::*;

fn int_op(name: &str, f: fn(i64, i64) -> Option<i64>) -> impl Fn(TestCase) {
    let name = name.to_string();
    move |tc: TestCase| {
        let a = tc.draw(nix_ints());
        let b = tc.draw(nix_ints());
        check(
            &format!(
                "builtins.{name} {} {}",
                nix_int_literal(a),
                nix_int_literal(b)
            ),
            f(a, b).ok_or(()),
        );
    }
}

// Integer overflow is an evaluation error in current Nix and Lix.
#[hegel::test]
fn add_int(tc: TestCase) {
    int_op("add", i64::checked_add)(tc)
}

#[hegel::test]
fn sub_int(tc: TestCase) {
    int_op("sub", i64::checked_sub)(tc)
}

#[hegel::test]
fn mul_int(tc: TestCase) {
    int_op("mul", i64::checked_mul)(tc)
}

/// Division truncates towards zero; dividing by zero and `MIN / -1` fail.
#[hegel::test]
fn div_int(tc: TestCase) {
    int_op("div", i64::checked_div)(tc)
}

#[hegel::test]
fn bit_and(tc: TestCase) {
    int_op("bitAnd", |a, b| Some(a & b))(tc)
}

#[hegel::test]
fn bit_or(tc: TestCase) {
    int_op("bitOr", |a, b| Some(a | b))(tc)
}

#[hegel::test]
fn bit_xor(tc: TestCase) {
    int_op("bitXor", |a, b| Some(a ^ b))(tc)
}

/// An int or a float, rendered as a literal, with its value as an f64.
fn number(tc: &TestCase) -> (String, NixValue) {
    if tc.draw(gs::booleans()) {
        let i = tc.draw(nix_ints());
        (nix_int_literal(i), NixValue::Int(i))
    } else {
        let f = tc.draw(nix_floats());
        (nix_float_literal(f), NixValue::Float(f))
    }
}

fn as_f64(v: &NixValue) -> f64 {
    match v {
        NixValue::Int(i) => *i as f64,
        NixValue::Float(f) => *f,
        _ => unreachable!(),
    }
}

/// Mixed int/float arithmetic promotes to float. Pure-int cases are covered
/// above, so at least one side is a float here.
#[hegel::test]
fn float_arith(tc: TestCase) {
    let (la, a) = number(&tc);
    let (lb, b) = number(&tc);
    tc.assume(matches!(a, NixValue::Float(_)) || matches!(b, NixValue::Float(_)));
    let op = tc.draw(gs::sampled_from(vec!["+", "-", "*", "/"]));
    let (x, y) = (as_f64(&a), as_f64(&b));
    let r = match op {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        _ => x / y,
    };
    let expect = if op == "/" && y == 0.0 {
        Expect::Error
    } else if r.is_finite() {
        Expect::Value(NixValue::Float(r).to_json())
    } else {
        // How to print an infinity as JSON isn't something to model.
        Expect::Unspecified
    };
    check(&format!("{la} {op} {lb}"), expect);
}

/// Comparing an int with a float converts the int to a float.
#[hegel::test]
fn less_than(tc: TestCase) {
    let (la, a) = number(&tc);
    let (lb, b) = number(&tc);
    let want = match (&a, &b) {
        (NixValue::Int(x), NixValue::Int(y)) => x < y,
        _ => as_f64(&a) < as_f64(&b),
    };
    check(&format!("builtins.lessThan {la} {lb}"), Expect::value(want));
}

/// `floor`/`ceil` of a float whose result doesn't fit in an int should be an
/// error rather than a wrapped or saturated value.
fn rounding(tc: TestCase, name: &str, f: fn(f64) -> f64) {
    let x = tc.draw(hegel::one_of!(
        nix_floats(),
        gs::floats::<f64>().min_value(-1e19).max_value(1e19),
    ));
    // CppNix and Lix reject subnormal literals; fix accepts them.
    if x != 0.0 && x.abs() < f64::MIN_POSITIVE {
        skip_known(&tc, "subnormal-literals");
        check(
            &format!("builtins.{name} {}", nix_float_literal(x)),
            Expect::Error,
        );
        return;
    }
    let r = f(x);
    let expect = if (-9223372036854775808.0..9223372036854775808.0).contains(&r) {
        Expect::value(r as i64)
    } else {
        // fix returns i64::MIN.
        skip_known(&tc, "float-to-int-range");
        Expect::Error
    };
    check(&format!("builtins.{name} {}", nix_float_literal(x)), expect);
}

#[hegel::test]
fn floor(tc: TestCase) {
    rounding(tc, "floor", f64::floor)
}

#[hegel::test]
fn ceil(tc: TestCase) {
    rounding(tc, "ceil", f64::ceil)
}

/// Integer literals beyond `i64::MAX` are rejected.
#[hegel::test]
fn int_literal_range(tc: TestCase) {
    let n = tc.draw(hegel::one_of!(
        gs::integers::<u64>(),
        gs::integers::<u64>()
            .min_value(i64::MAX as u64 - 2)
            .max_value(i64::MAX as u64 + 2),
    ));
    let expect: Expect = i64::try_from(n).into();
    check(&n.to_string(), expect);
}

/// Every float literal (including subnormals like `5.0e-324`) should parse
/// to the float it denotes. Differential only: CppNix and Lix reject
/// subnormal literals, so there is no agreed-upon model.
#[hegel::test]
fn subnormal_float_literal(tc: TestCase) {
    // fix accepts them.
    if is_skipped("subnormal-literals") {
        return;
    }
    let f = tc.draw(
        gs::floats::<f64>()
            .min_value(f64::MIN_POSITIVE / 2.0f64.powi(52))
            .max_value_exclusive(f64::MIN_POSITIVE),
    );
    check(&nix_float_literal(f), Expect::Unspecified);
}
