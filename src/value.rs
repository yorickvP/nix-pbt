//! A model of the (JSON-representable) Nix values, and rendering to Nix syntax.

use serde_json::{Number, Value as Json};
use std::collections::BTreeMap;
use std::fmt;

/// A Nix value without functions, paths or derivations.
///
/// Attribute sets are `BTreeMap`s, whose byte-wise key order matches the
/// order Nix sorts attribute names in.
#[derive(Clone, PartialEq)]
pub enum NixValue {
    Null,
    Bool(bool),
    Int(i64),
    /// Always finite and never `-0.0`: neither is expressible as a literal.
    Float(f64),
    String(String),
    List(Vec<NixValue>),
    Attrs(BTreeMap<String, NixValue>),
}

impl NixValue {
    /// Render as a Nix expression that evaluates to this value.
    pub fn to_nix(&self) -> String {
        match self {
            NixValue::Null => "null".into(),
            NixValue::Bool(b) => b.to_string(),
            NixValue::Int(i) => nix_int_literal(*i),
            NixValue::Float(f) => nix_float_literal(*f),
            NixValue::String(s) => nix_string_literal(s),
            NixValue::List(xs) => {
                let mut s = String::from("[");
                for x in xs {
                    s.push(' ');
                    s.push_str(&x.to_nix());
                }
                s.push_str(" ]");
                s
            }
            NixValue::Attrs(m) => {
                let mut s = String::from("{");
                for (k, v) in m {
                    s.push(' ');
                    s.push_str(&nix_string_literal(k));
                    s.push_str(" = ");
                    s.push_str(&v.to_nix());
                    s.push(';');
                }
                s.push_str(" }");
                s
            }
        }
    }

    /// The JSON that `nix eval --json` prints for this value.
    pub fn to_json(&self) -> Json {
        match self {
            NixValue::Null => Json::Null,
            NixValue::Bool(b) => Json::Bool(*b),
            NixValue::Int(i) => Json::from(*i),
            NixValue::Float(f) => Json::Number(Number::from_f64(*f).expect("finite float")),
            NixValue::String(s) => Json::String(s.clone()),
            NixValue::List(xs) => Json::Array(xs.iter().map(NixValue::to_json).collect()),
            NixValue::Attrs(m) => {
                Json::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect())
            }
        }
    }

    /// The name `builtins.typeOf` returns.
    pub fn type_name(&self) -> &'static str {
        match self {
            NixValue::Null => "null",
            NixValue::Bool(_) => "bool",
            NixValue::Int(_) => "int",
            NixValue::Float(_) => "float",
            NixValue::String(_) => "string",
            NixValue::List(_) => "list",
            NixValue::Attrs(_) => "set",
        }
    }
}

/// Nix's `==`: like structural equality, except that ints and floats compare
/// numerically (`1 == 1.0`).
pub fn nix_eq(a: &NixValue, b: &NixValue) -> bool {
    use NixValue::*;
    match (a, b) {
        (Int(x), Float(y)) | (Float(y), Int(x)) => *x as f64 == *y,
        (List(xs), List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| nix_eq(x, y))
        }
        (Attrs(xs), Attrs(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((kx, x), (ky, y))| kx == ky && nix_eq(x, y))
        }
        _ => a == b,
    }
}

impl fmt::Debug for NixValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_nix())
    }
}

/// An integer literal. Negative numbers are parenthesised so the result can
/// be used as a function argument or list element, and `i64::MIN` (whose
/// magnitude has no literal) is built by subtraction.
pub fn nix_int_literal(i: i64) -> String {
    if i == i64::MIN {
        "(-9223372036854775807 - 1)".into()
    } else if i < 0 {
        format!("(-{})", i.unsigned_abs())
    } else {
        i.to_string()
    }
}

/// A float literal that parses back to exactly `f`.
///
/// Nix float literals need a `.` in the mantissa (`1e3` lexes as `1`
/// applied to the variable `e3`), so we print the shortest round-tripping
/// scientific form and patch in a `.0` where needed.
pub fn nix_float_literal(f: f64) -> String {
    assert!(f.is_finite(), "non-finite floats have no Nix literal");
    let s = format!("{:e}", f.abs());
    let (mantissa, exp) = s.split_once('e').unwrap();
    let mantissa = if mantissa.contains('.') {
        mantissa.to_string()
    } else {
        format!("{mantissa}.0")
    };
    let lit = format!("{mantissa}e{exp}");
    if f.is_sign_negative() && f != 0.0 {
        format!("(-{lit})")
    } else {
        lit
    }
}

/// A double-quoted string literal. `$` is always escaped so that `${` can't
/// start an interpolation, and `\r` is escaped because Nix normalises raw
/// carriage returns in string literals.
pub fn nix_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => panic!("Nix strings cannot contain NUL"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render a list of already-rendered Nix expressions.
pub fn nix_list<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut s = String::from("[");
    for i in items {
        s.push(' ');
        s.push_str(&i);
    }
    s.push_str(" ]");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals() {
        assert_eq!(nix_int_literal(-3), "(-3)");
        assert_eq!(nix_float_literal(1e300), "1.0e300");
        assert_eq!(nix_float_literal(-0.1), "(-1.0e-1)");
        assert_eq!(nix_float_literal(0.0), "0.0e0");
        assert_eq!(nix_string_literal("a${b}\"\\"), r#""a\${b}\"\\""#);
    }
}
