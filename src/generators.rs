//! Hegel generators for Nix values.

use crate::value::NixValue;
use hegel::generators::{self as gs, Generator, PrintableGenerator};
use hegel::{PrettyPrintable, PrettyPrinter};

impl PrettyPrintable for NixValue {
    fn pretty_print(&self, printer: &mut PrettyPrinter) {
        printer.text(&self.to_nix());
    }
}

/// Characters that are "interesting" to Nix: separators used by
/// `splitVersion`, escape-relevant characters, and a multi-byte character.
const SPICY: &str = "ab01.-_ $\"\\{}\n\té";

/// Strings suitable for Nix: any Unicode except NUL, biased towards short
/// strings over a small alphabet so that collisions and escapes are common.
pub fn nix_strings() -> impl PrintableGenerator<String> {
    hegel::one_of!(
        gs::text().alphabet(SPICY).max_size(8),
        gs::text().exclude_characters("\0").max_size(12),
    )
}

/// ASCII-only strings, for builtins whose byte-vs-character semantics we
/// don't want to model.
pub fn ascii_strings() -> impl PrintableGenerator<String> {
    gs::text().min_codepoint(1).max_codepoint(0x7f).max_size(12)
}

/// Attribute names. `outPath` and `__toString` are excluded because they
/// change how an attribute set is serialised to JSON.
pub fn attr_names() -> impl PrintableGenerator<String> {
    hegel::one_of!(gs::text().alphabet("abc").max_size(3), nix_strings(),)
        .filter(|s: &String| s != "outPath" && s != "__toString")
}

/// Integers, biased towards small values but covering the full `i64` range.
pub fn nix_ints() -> impl PrintableGenerator<i64> {
    hegel::one_of!(
        gs::integers::<i64>().min_value(-10).max_value(10),
        gs::integers::<i64>(),
    )
}

/// Finite floats other than `-0.0`, which Nix has no literal for.
///
/// Subnormals are excluded: CppNix and Lix reject subnormal literals with
/// "invalid float" (see the `subnormal_float_literal` test).
pub fn nix_floats() -> impl PrintableGenerator<f64> {
    gs::floats::<f64>()
        .allow_nan(false)
        .allow_infinity(false)
        .allow_subnormal(false)
        .map(|f| if f == 0.0 { 0.0 } else { f })
}

/// Non-container Nix values.
pub fn nix_scalars() -> impl PrintableGenerator<NixValue> {
    hegel::one_of!(
        gs::just(NixValue::Null),
        gs::booleans().map(NixValue::Bool),
        nix_ints().map(NixValue::Int),
        nix_floats().map(NixValue::Float),
        nix_strings().map(NixValue::String),
    )
}

/// Arbitrary (JSON-representable) Nix values, nested a few levels deep.
pub fn nix_values() -> impl PrintableGenerator<NixValue> {
    gs::recursive(nix_scalars(), |sub| {
        hegel::one_of!(
            gs::vecs(sub.clone()).max_size(4).map(NixValue::List),
            gs::btree_maps(attr_names(), sub)
                .max_size(4)
                .map(NixValue::Attrs),
        )
    })
    .max_depth(4)
}
