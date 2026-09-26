//! Models of Nix's store path computations: nix32, `compressHash`,
//! `makeStorePath`, text paths (`builtins.toFile`, `.drv` files), output
//! paths of input-addressed derivations (`hashDerivationModulo`), and
//! `builtins.placeholder`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const STORE_DIR: &str = "/nix/store";

/// `StorePath::MaxNameLen`.
pub const MAX_NAME_LEN: usize = 211;

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Nix's base-32 encoding (`HashFormat::Nix32`): its own alphabet, and the
/// bytes are consumed from the end.
pub fn nix32(hash: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";
    let len = (hash.len() * 8).div_ceil(5);
    (0..len)
        .rev()
        .map(|n| {
            let b = n * 5;
            let (i, j) = (b / 8, b % 8);
            let lo = (hash[i] as u16) >> j;
            let hi = if i + 1 < hash.len() {
                (hash[i + 1] as u16) << (8 - j)
            } else {
                0
            };
            CHARS[((lo | hi) & 0x1f) as usize] as char
        })
        .collect()
}

/// `compressHash`: XOR-fold a hash down to `size` bytes.
pub fn compress_hash(hash: &[u8], size: usize) -> Vec<u8> {
    let mut out = vec![0u8; size];
    for (i, b) in hash.iter().enumerate() {
        out[i % size] ^= b;
    }
    out
}

/// `checkName` from src/libstore/path.cc.
pub fn check_name(name: &str) -> Result<(), String> {
    let b = name.as_bytes();
    if b.is_empty() {
        return Err("name must not be empty".into());
    }
    if b.len() > MAX_NAME_LEN {
        return Err(format!("name longer than {MAX_NAME_LEN}"));
    }
    if b[0] == b'.'
        && (b.len() == 1 || b[1] == b'-' || (b[1] == b'.' && (b.len() == 2 || b[2] == b'-')))
    {
        return Err(format!("name '{name}' is not valid"));
    }
    match b
        .iter()
        .find(|&&c| !(c.is_ascii_alphanumeric() || b"+-._?=".contains(&c)))
    {
        Some(c) => Err(format!("illegal character {:?}", *c as char)),
        None => Ok(()),
    }
}

/// `makeStorePath(type, hash, name)`.
pub fn make_store_path(ty: &str, hash: &[u8; 32], name: &str) -> String {
    let s = format!("{ty}:sha256:{}:{STORE_DIR}:{name}", hex(hash));
    format!(
        "{STORE_DIR}/{}-{name}",
        nix32(&compress_hash(&sha256(s.as_bytes()), 20))
    )
}

/// `makeTextPath`: where `builtins.toFile` and `.drv` files end up.
/// References must be store paths.
pub fn text_path(name: &str, contents: &[u8], references: &[String]) -> String {
    let mut refs = references.to_vec();
    refs.sort();
    refs.dedup();
    let mut ty = "text".to_string();
    for r in &refs {
        ty.push(':');
        ty.push_str(r);
    }
    make_store_path(&ty, &sha256(contents), name)
}

/// `builtins.placeholder`.
pub fn placeholder(output: &str) -> String {
    format!(
        "/{}",
        nix32(&sha256(format!("nix-output:{output}").as_bytes()))
    )
}

/// An input-addressed derivation without input derivations or sources: the
/// kind `derivation { ... }` produces from plain strings.
#[derive(Clone, Debug)]
pub struct Derivation {
    pub name: String,
    pub outputs: Vec<String>,
    pub platform: String,
    pub builder: String,
    pub args: Vec<String>,
    /// Without the output paths, which are filled in by [`Self::paths`].
    pub env: BTreeMap<String, String>,
}

/// String syntax in the ATerm format (`printString` in derivations.cc).
fn aterm_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn aterm_list<T>(out: &mut String, items: &[T], mut f: impl FnMut(&mut String, &T)) {
    out.push('[');
    for (i, x) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        f(out, x);
    }
    out.push(']');
}

/// Output paths and the `.drv` path of a derivation.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivationPaths {
    pub drv_path: String,
    pub outputs: BTreeMap<String, String>,
}

impl Derivation {
    /// `Derivation::unparse`: the ATerm text of the `.drv` file, with the
    /// given output paths (empty strings when masked for hashing).
    pub fn aterm(&self, output_paths: &BTreeMap<String, String>) -> String {
        let mut env = self.env.clone();
        for (id, path) in output_paths {
            env.insert(id.clone(), path.clone());
        }
        let outputs: Vec<(&String, &String)> = output_paths.iter().collect();
        let env: Vec<(&String, &String)> = env.iter().collect();
        let mut s = String::from("Derive(");
        aterm_list(&mut s, &outputs, |s, (id, path)| {
            s.push('(');
            aterm_string(s, id);
            s.push(',');
            aterm_string(s, path);
            s.push_str(",\"\",\"\")");
        });
        s.push_str(",[],[],");
        // The platform is printed without escaping (`printUnquotedString`
        // in aterm.cc): it's meant to come from a restricted alphabet.
        s.push('"');
        s.push_str(&self.platform);
        s.push('"');
        s.push(',');
        aterm_string(&mut s, &self.builder);
        s.push(',');
        aterm_list(&mut s, &self.args, |s, a| aterm_string(s, a));
        s.push(',');
        aterm_list(&mut s, &env, |s, (k, v)| {
            s.push('(');
            aterm_string(s, k);
            s.push(',');
            aterm_string(s, v);
            s.push(')');
        });
        s.push(')');
        s
    }

    /// Compute the output paths (from `hashDerivationModulo` of the
    /// derivation with its outputs masked) and then the `.drv` path.
    pub fn paths(&self) -> DerivationPaths {
        let masked: BTreeMap<String, String> = self
            .outputs
            .iter()
            .map(|o| (o.clone(), String::new()))
            .collect();
        let hash = sha256(self.aterm(&masked).as_bytes());
        let outputs: BTreeMap<String, String> = self
            .outputs
            .iter()
            .map(|id| {
                let name = if id == "out" {
                    self.name.clone()
                } else {
                    format!("{}-{id}", self.name)
                };
                (
                    id.clone(),
                    make_store_path(&format!("output:{id}"), &hash, &name),
                )
            })
            .collect();
        let drv_path = text_path(
            &format!("{}.drv", self.name),
            self.aterm(&outputs).as_bytes(),
            &[],
        );
        DerivationPaths { drv_path, outputs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_paths() {
        // Checked against CppNix, Lix and fix.
        assert_eq!(
            text_path("hello", b"world", &[]),
            "/nix/store/a8pm5wln4zaphc7x9iaqrgm9fravifib-hello"
        );
        assert_eq!(
            placeholder("out"),
            "/1rz4g4znpzjwh1xymhjpm42vipw92pr73vdgl6xs1hycac8kf2n9"
        );
        let env = BTreeMap::from(
            [
                ("name", "x"),
                ("system", "x86_64-linux"),
                ("builder", "/bin/sh"),
                ("n", "1"),
                ("b", "1"),
                ("f", ""),
                ("nul", ""),
                ("l", "1 a 1"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let drv = Derivation {
            name: "x".into(),
            outputs: vec!["out".into()],
            platform: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            args: vec!["-c".into(), "echo".into()],
            env,
        };
        let p = drv.paths();
        assert_eq!(
            p.drv_path,
            "/nix/store/cl7xa3v0d0bw7xc5fm60wl03nxp3kj9a-x.drv"
        );
        assert_eq!(
            p.outputs["out"],
            "/nix/store/0ls03mjdhihiddqff8c2fbwgmc6lr5qz-x"
        );
    }
}
