//! Models of Nix's store path computations: nix32, `compressHash`,
//! `makeStorePath`, text paths (`builtins.toFile`, `.drv` files), output
//! paths of input-addressed derivations (`hashDerivationModulo`) and of
//! fixed-output ones (`makeFixedOutputPath`), `builtins.placeholder`, and
//! the hash encodings Nix reads (`Hash::parseAny`).

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

const NIX32_CHARS: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Decode Nix's base-32 (`BaseNix32::decode`): bits that don't fit in
/// `size` bytes must be zero.
pub fn nix32_decode(s: &str, size: usize) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut hash = vec![0u8; size];
    for n in 0..b.len() {
        let c = b[b.len() - n - 1];
        let digit = NIX32_CHARS.iter().position(|&x| x == c)? as u16;
        let bit = n * 5;
        let (i, j) = (bit / 8, bit % 8);
        if i >= size {
            return None;
        }
        hash[i] |= (digit << j) as u8;
        let carry = digit >> (8 - j);
        if i + 1 < size {
            hash[i + 1] |= carry as u8;
        } else if carry != 0 {
            return None;
        }
    }
    Some(hash)
}

const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base-64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut data, mut nbits) = (0u32, 0u32);
    for &c in bytes {
        data = data << 8 | c as u32;
        nbits += 8;
        while nbits >= 6 {
            nbits -= 6;
            out.push(BASE64_CHARS[(data >> nbits & 0x3f) as usize] as char);
        }
    }
    if nbits > 0 {
        out.push(BASE64_CHARS[(data << (6 - nbits) & 0x3f) as usize] as char);
    }
    while out.len() % 4 != 0 {
        out.push('=');
    }
    out
}

/// `base64::decode`: stops at the first `=`, so padding is optional, and
/// rejects any other character outside the alphabet.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut d, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        if c == b'\n' {
            continue;
        }
        let digit = BASE64_CHARS.iter().position(|&x| x == c)? as u32;
        bits += 6;
        d = d << 6 | digit;
        if bits >= 8 {
            out.push((d >> (bits - 8) & 0xff) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// The hash algorithms Nix knows (without experimental features), with
/// their digest sizes.
pub fn hash_size(algo: &str) -> Option<usize> {
    match algo {
        "md5" => Some(16),
        "sha1" => Some(20),
        "sha256" => Some(32),
        "sha512" => Some(64),
        _ => None,
    }
}

/// `Hash::parseAny`: `algo:rest` or SRI `algo-base64`, or a bare hash of
/// `algo`, whose encoding its length tells (base-16 in either case, nix32,
/// padded base-64). Returns the algorithm and the digest.
pub fn parse_any_hash(text: &str, algo: Option<&str>) -> Result<(String, Vec<u8>), String> {
    let (prefix, rest, sri) = if let Some(i) = text.find(':') {
        (Some(&text[..i]), &text[i + 1..], false)
    } else if let Some(i) = text.find('-') {
        (Some(&text[..i]), &text[i + 1..], true)
    } else {
        (None, text, false)
    };
    if let Some(p) = prefix {
        if hash_size(p).is_none() {
            return Err(format!("unknown hash algorithm '{p}'"));
        }
    }
    let algo = match (prefix, algo) {
        (None, None) => return Err(format!("hash '{text}' does not include a type")),
        (Some(p), Some(a)) if p != a => {
            return Err(format!("hash '{text}' should have type '{a}'"));
        }
        (Some(p), _) => p,
        (None, Some(a)) => a,
    };
    let size = hash_size(algo).ok_or_else(|| format!("unknown hash algorithm '{algo}'"))?;
    let decoded = if sri {
        base64_decode(rest)
    } else if rest.len() == 2 * size {
        (0..size)
            .map(|i| u8::from_str_radix(&rest[2 * i..2 * i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()
            .filter(|_| rest.bytes().all(|c| c.is_ascii_hexdigit()))
    } else if rest.len() == (size * 8).div_ceil(5) {
        nix32_decode(rest, size)
    } else if rest.len() == size.div_ceil(3) * 4 {
        base64_decode(rest)
    } else {
        return Err(format!(
            "hash '{rest}' has wrong length for hash algorithm '{algo}'"
        ));
    };
    match decoded {
        Some(d) if d.len() == size => Ok((algo.to_string(), d)),
        _ => Err(format!("invalid hash '{text}'")),
    }
}

/// How a fixed-output derivation's content is hashed.
#[derive(Clone, Debug)]
pub struct FixedOutput {
    /// `outputHashMode = "recursive"` (or `"nar"`): a NAR hash.
    pub recursive: bool,
    pub algo: String,
    pub digest: Vec<u8>,
}

impl FixedOutput {
    /// The `.drv`'s hash algorithm field: `r:sha256`, `sha1`, ...
    fn hash_algo(&self) -> String {
        format!("{}{}", if self.recursive { "r:" } else { "" }, self.algo)
    }
}

/// `makeFixedOutputPath`: a NAR hash with SHA-256 is a `source` path, like
/// `builtins.path` makes; anything else goes through a `fixed:out:` string.
pub fn fixed_output_path(name: &str, fixed: &FixedOutput) -> String {
    if fixed.recursive && fixed.algo == "sha256" {
        make_store_path("source", fixed.digest.as_slice().try_into().unwrap(), name)
    } else {
        let s = format!("fixed:out:{}:{}:", fixed.hash_algo(), hex(&fixed.digest));
        make_store_path("output:out", &sha256(s.as_bytes()), name)
    }
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
    /// A fixed-output derivation (whose only output is `out`).
    pub fixed: Option<FixedOutput>,
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
        let (hash_algo, hash) = match &self.fixed {
            Some(f) => (f.hash_algo(), hex(&f.digest)),
            None => (String::new(), String::new()),
        };
        aterm_list(&mut s, &outputs, |s, (id, path)| {
            s.push('(');
            aterm_string(s, id);
            s.push(',');
            aterm_string(s, path);
            s.push(',');
            aterm_string(s, &hash_algo);
            s.push(',');
            aterm_string(s, &hash);
            s.push(')');
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
        if let Some(fixed) = &self.fixed {
            let outputs =
                BTreeMap::from([("out".to_string(), fixed_output_path(&self.name, fixed))]);
            let drv_path = text_path(
                &format!("{}.drv", self.name),
                self.aterm(&outputs).as_bytes(),
                &[],
            );
            return DerivationPaths { drv_path, outputs };
        }
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
            fixed: None,
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

    #[test]
    fn known_fixed_output_paths() {
        // Checked against CppNix.
        let fixed = |recursive, algo: Option<&str>| {
            let mut env = BTreeMap::from(
                [("name", "a"), ("system", "x"), ("builder", "/b")]
                    .map(|(k, v)| (k.to_string(), v.to_string())),
            );
            env.insert(
                "outputHash".into(),
                if recursive {
                    "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
                } else {
                    ""
                }
                .into(),
            );
            if let Some(a) = algo {
                env.insert("outputHashAlgo".into(), a.into());
            }
            if recursive {
                env.insert("outputHashMode".into(), "recursive".into());
            }
            Derivation {
                name: "a".into(),
                outputs: vec!["out".into()],
                platform: "x".into(),
                builder: "/b".into(),
                args: vec![],
                env,
                fixed: Some(FixedOutput {
                    recursive,
                    algo: "sha256".into(),
                    digest: vec![0; 32],
                }),
            }
        };
        assert_eq!(
            fixed(true, None).paths().outputs["out"],
            "/nix/store/pkprgjg3irwr8bbfp4rfjvd1qbqpbpa4-a"
        );
        assert_eq!(
            fixed(false, Some("sha256")).paths().drv_path,
            "/nix/store/x3q0fasrd4mzy94mjknrcxkn5zi4ysrp-a.drv"
        );
    }

    #[test]
    fn hash_encodings_round_trip() {
        let digest: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37)).collect();
        assert_eq!(nix32_decode(&nix32(&digest), 32).unwrap(), digest);
        assert_eq!(base64_decode(&base64(&digest)).unwrap(), digest);
        for text in [
            hex(&digest),
            hex(&digest).to_uppercase(),
            nix32(&digest),
            base64(&digest),
            format!("sha256-{}", base64(&digest)),
            format!("sha256-{}", base64(&digest).trim_end_matches('=')),
            format!("sha256:{}", nix32(&digest)),
        ] {
            let algo = if text.starts_with("sha256") {
                None
            } else {
                Some("sha256")
            };
            assert_eq!(parse_any_hash(&text, algo).unwrap().1, digest, "{text}");
        }
        assert!(parse_any_hash(&hex(&digest), None).is_err());
        assert!(parse_any_hash(&format!("sha1:{}", hex(&digest)), None).is_err());
        assert!(parse_any_hash(&format!("sha256:{}", hex(&digest)), Some("sha1")).is_err());
        assert!(parse_any_hash("foo-bar", None).is_err());
        // Bits past the digest must be zero.
        assert!(nix32_decode(&"z".repeat(52), 32).is_none());
    }
}
