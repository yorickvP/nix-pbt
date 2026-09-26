//! Fetchers and source copying on generated file trees.
//!
//! The trees are "angry", in the spirit of
//! [angryfiles](https://github.com/jakeogh/angryfiles): names with every
//! kind of byte (newlines, invalid UTF-8, NFD vs NFC, 255-byte names),
//! symlinks (dangling, absolute, to themselves), executable bits, empty
//! directories. Each tree goes through
//!
//! - `builtins.path` (with and without a `filter`) and `filterSource`,
//! - `fetchTree` of type `path`, `git` (clean and dirty) and `tarball`, and
//!   `fetchTarball`,
//! - `fetchurl` of a single file,
//! - a walk of the tree and of its store copies with `readDir`,
//!   `readFileType` and `hashFile`,
//!
//! and is compared against a model of the NAR serialisation and the
//! resulting store paths. `angry_corpus` does the same for a fixed tree with
//! every one-byte name and every name length.

use hegel::TestCase;
use hegel::generators::{self as gs, Generator};
use nix_pbt::store::{hex, make_store_path, sha256};
use nix_pbt::*;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// The model

#[derive(Clone, Debug, PartialEq)]
enum Node {
    File { contents: Vec<u8>, exec: bool },
    Symlink(Vec<u8>),
    Dir(Tree),
}

/// Sorted by name bytes, like NAR directory entries.
type Tree = BTreeMap<Vec<u8>, Node>;

fn nar_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend((s.len() as u64).to_le_bytes());
    out.extend(s);
    out.extend(std::iter::repeat_n(0, (8 - s.len() % 8) % 8));
}

fn nar_node(out: &mut Vec<u8>, node: &Node) {
    nar_str(out, b"(");
    nar_str(out, b"type");
    match node {
        Node::File { contents, exec } => {
            nar_str(out, b"regular");
            if *exec {
                nar_str(out, b"executable");
                nar_str(out, b"");
            }
            nar_str(out, b"contents");
            nar_str(out, contents);
        }
        Node::Symlink(target) => {
            nar_str(out, b"symlink");
            nar_str(out, b"target");
            nar_str(out, target);
        }
        Node::Dir(entries) => {
            nar_str(out, b"directory");
            for (name, node) in entries {
                nar_str(out, b"entry");
                nar_str(out, b"(");
                nar_str(out, b"name");
                nar_str(out, name);
                nar_str(out, b"node");
                nar_node(out, node);
                nar_str(out, b")");
            }
        }
    }
    nar_str(out, b")");
}

fn nar_hash(node: &Node) -> [u8; 32] {
    let mut out = Vec::new();
    nar_str(&mut out, b"nix-archive-1");
    nar_node(&mut out, node);
    sha256(&out)
}

fn base64(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, &b| (n << 8) | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(CHARS[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `narHash` as fetchers report it.
fn sri(node: &Node) -> String {
    format!("sha256-{}", base64(&nar_hash(node)))
}

/// Where a NAR-hashed source ends up (`makeFixedOutputPath` with
/// `FileIngestionMethod::NixArchive`, SHA-256, no references).
fn source_path(node: &Node, name: &str) -> String {
    make_store_path("source", &nar_hash(node), name)
}

/// Where `fetchurl` puts a file (flat SHA-256).
fn flat_path(contents: &[u8], name: &str) -> String {
    let fp = format!("fixed:out:sha256:{}:", hex(&sha256(contents)));
    make_store_path("output:out", &sha256(fp.as_bytes()), name)
}

/// What git can represent: no empty directories.
fn without_empty_dirs(tree: &Tree) -> Tree {
    tree.iter()
        .filter_map(|(name, node)| match node {
            Node::Dir(sub) => {
                let sub = without_empty_dirs(sub);
                (!sub.is_empty()).then(|| (name.clone(), Node::Dir(sub)))
            }
            other => Some((name.clone(), other.clone())),
        })
        .collect()
}

/// The result of [`WALK`] on a tree.
fn walk_model(node: &Node) -> Json {
    match node {
        Node::File { contents, .. } => json!(["regular", hex(&sha256(contents))]),
        Node::Symlink(_) => json!(["symlink"]),
        Node::Dir(entries) => Json::Array(
            entries
                .iter()
                .map(|(name, node)| json!([hex(&sha256(name)), walk_model(node)]))
                .collect(),
        ),
    }
}

/// Walks a directory with `readDir`, `readFileType` and `hashFile`. Names
/// are hashed: they needn't be valid UTF-8, which JSON can't carry.
const WALK: &str = "let walk = p: t: \
    if t == \"directory\" then \
      let es = builtins.readDir p; in \
      map (n: [ (builtins.hashString \"sha256\" n) (walk (p + \"/${n}\") es.${n}) ]) (builtins.attrNames es) \
    else if t == \"regular\" then [ \"regular\" (builtins.hashFile \"sha256\" p) ] \
    else [ t ]; in walk";

// ---------------------------------------------------------------------------
// Generation

/// Names that tend to break things. `.git*` names are left out: git won't
/// store them.
const ANGRY_NAMES: &[&[u8]] = &[
    b"a",
    b"A",
    b"b",
    b" ",
    b"a b",
    b" a",
    b"a ",
    b"-",
    b"--help",
    b"~",
    b"$HOME",
    b"${x}",
    b"`x`",
    b"*",
    b"?",
    b"[a]",
    b"\\",
    b"'",
    b"\"",
    b"\n",
    b"a\nb",
    b"\t",
    b"\r",
    b"#",
    b"%",
    b"%20",
    b"%2F",
    b"&",
    b";",
    b"<>",
    b":",
    b"@",
    b".a",
    b"..a",
    b"a.",
    b"...",
    b"\x01",
    b"\x1b[31m",
    b"\x7f",
    b"\xff",
    b"\xc3",
    b"\xed\xa0\x80",
    "é".as_bytes(),
    "e\u{301}".as_bytes(),
    "😀".as_bytes(),
    "\u{202e}".as_bytes(),
    "\u{feff}".as_bytes(),
    b"default.nix",
    b"flake.nix",
];

const LINK_TARGETS: &[&[u8]] = &[
    b"a",
    b"../a",
    b"./a",
    b".",
    b"..",
    b"/",
    b"/nix/store",
    b"/nix-pbt-missing",
    b"missing",
    b"a/b/c",
    b"\n",
    b"\xff",
];

fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0)
        && !name.to_ascii_lowercase().starts_with(b".git")
}

struct Gen<'a> {
    tc: &'a TestCase,
}

impl Gen<'_> {
    fn chance(&self, p: f64) -> bool {
        self.tc.draw_silent(gs::weighted_booleans(p))
    }

    fn bytes(&self, max: usize) -> Vec<u8> {
        self.tc
            .draw_silent(gs::vecs(gs::integers::<u8>()).max_size(max))
    }

    fn name(&self) -> Vec<u8> {
        loop {
            let name = match self.tc.draw_silent(gs::integers::<u8>().max_value(9)) {
                0..=5 => self
                    .tc
                    .draw_silent(gs::sampled_from(ANGRY_NAMES.to_vec()))
                    .to_vec(),
                6 | 7 => self.bytes(6),
                8 => self.tc.draw_silent(gs::text().max_size(6)).into_bytes(),
                _ => vec![
                    b'n';
                    self.tc
                        .draw_silent(gs::integers::<usize>().min_value(200).max_value(255))
                ],
            };
            if is_valid_name(&name) {
                return name;
            }
        }
    }

    fn node(&self, depth: usize) -> Node {
        match self.tc.draw_silent(gs::integers::<u8>().max_value(9)) {
            0..=4 => Node::File {
                contents: if self.chance(0.3) {
                    Vec::new()
                } else {
                    self.bytes(40)
                },
                exec: self.chance(0.3),
            },
            5 | 6 => Node::Symlink(if self.chance(0.7) {
                self.tc
                    .draw_silent(gs::sampled_from(LINK_TARGETS.to_vec()))
                    .to_vec()
            } else {
                self.name()
            }),
            _ if depth == 0 => Node::Dir(Tree::new()),
            _ => Node::Dir(self.tree(depth - 1)),
        }
    }

    fn tree(&self, depth: usize) -> Tree {
        let n = self.tc.draw_silent(gs::integers::<usize>().max_value(4));
        (0..n).map(|_| (self.name(), self.node(depth))).collect()
    }
}

fn show_name(name: &[u8]) -> String {
    let mut s = String::from("\"");
    for chunk in name.utf8_chunks() {
        s.extend(chunk.valid().escape_debug());
        for b in chunk.invalid() {
            s.push_str(&format!("\\x{b:02x}"));
        }
    }
    s.push('"');
    s
}

fn show_tree(tree: &Tree, indent: usize, out: &mut String) {
    for (name, node) in tree {
        out.push_str(&" ".repeat(indent));
        out.push_str(&show_name(name));
        match node {
            Node::File { contents, exec } => out.push_str(&format!(
                " = file{} {}\n",
                if *exec { " (executable)" } else { "" },
                show_name(contents)
            )),
            Node::Symlink(target) => out.push_str(&format!(" -> {}\n", show_name(target))),
            Node::Dir(sub) => {
                out.push_str("/\n");
                show_tree(sub, indent + 2, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Materialising trees

/// Timestamp of every file, commit and tarball entry.
const MTIME: i64 = 1700000000;

/// A scratch directory, deleted afterwards unless the test failed or it's
/// marked to be kept.
struct Scratch(PathBuf, bool);

impl Scratch {
    fn new() -> Scratch {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = tmp_dir().join(format!(
            "nix-pbt-trees-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir, false)
    }

    /// Keep the directory. CppNix reads fetched git trees lazily from the
    /// repository they were first fetched from, even when the same tree is
    /// fetched from another one later (see `stale_git_mount`).
    fn keep(mut self) -> Scratch {
        self.1 = true;
        self
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("trees kept at {}", self.0.display());
        } else if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn write_tree(dir: &Path, tree: &Tree) {
    std::fs::create_dir(dir).unwrap();
    for (name, node) in tree {
        let path = dir.join(OsStr::from_bytes(name));
        match node {
            Node::File { contents, exec } => {
                std::fs::write(&path, contents).unwrap();
                let mode = if *exec { 0o755 } else { 0o644 };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            }
            Node::Symlink(target) => {
                std::os::unix::fs::symlink(OsStr::from_bytes(target), &path).unwrap()
            }
            Node::Dir(sub) => write_tree(&path, sub),
        }
    }
}

fn run(cmd: &mut Command) -> String {
    let out = cmd
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "a")
        .env("GIT_AUTHOR_EMAIL", "a@a")
        .env("GIT_COMMITTER_NAME", "a")
        .env("GIT_COMMITTER_EMAIL", "a@a")
        .env("GIT_AUTHOR_DATE", format!("@{MTIME} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{MTIME} +0000"))
        .output()
        .unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Set every mtime (and the symlinks' own) to [`MTIME`].
fn touch_all(dir: &Path) {
    run(Command::new("find").arg(dir).args([
        "-exec",
        "touch",
        "-h",
        "-d",
        &format!("@{MTIME}"),
        "{}",
        "+",
    ]));
}

/// A string literal for a path in the scratch directory.
fn lit(path: &Path) -> String {
    nix_string_literal(path.to_str().expect("scratch paths are UTF-8"))
}

// ---------------------------------------------------------------------------
// Checks

/// `builtins.path`, `filterSource`, `fetchTree { type = "path"; }`, and a
/// walk of the tree and its copies.
fn check_path_copies(tc: Option<&TestCase>, dir: &Path, tree: &Tree) {
    let node = Node::Dir(tree.clone());
    let p = format!("(/. + {})", lit(dir));
    check(
        &format!("builtins.path {{ path = {p}; name = \"tree\"; }}"),
        Expect::value(source_path(&node, "tree")),
    );
    check(
        &format!("{WALK} {p} \"directory\""),
        Expect::Value(walk_model(&node)),
    );
    // fix names the store path after the directory, not `source`.
    let (fields, want) = if is_skipped("fetch-path-name") {
        ("[ r.narHash ]", json!([sri(&node)]))
    } else {
        (
            "[ r.outPath r.narHash ]",
            json!([source_path(&node, "source"), sri(&node)]),
        )
    };
    check(
        &format!(
            "let r = builtins.fetchTree {{ type = \"path\"; path = {}; }}; in {fields}",
            lit(dir)
        ),
        Expect::Value(want),
    );
    check(
        &format!(
            "builtins.attrNames (builtins.fetchTree {{ type = \"path\"; path = {}; }})",
            lit(dir)
        ),
        Expect::Unspecified,
    );
    check(
        &format!("{WALK} (builtins.path {{ path = {p}; name = \"tree\"; }}) \"directory\""),
        Expect::Value(walk_model(&node)),
    );

    // Filters.
    type Filter = fn(&[u8], &Node) -> bool;
    let filters: Vec<(&str, Filter)> = vec![
        ("t != \"symlink\"", |_, n| !matches!(n, Node::Symlink(_))),
        ("t != \"directory\"", |_, n| !matches!(n, Node::Dir(_))),
        ("t == \"regular\" || t == \"directory\"", |_, n| {
            matches!(n, Node::File { .. } | Node::Dir(_))
        }),
        (
            "builtins.substring 0 1 (baseNameOf p) != \"a\"",
            |name, _| !name.starts_with(b"a"),
        ),
        ("builtins.stringLength (baseNameOf p) < 3", |name, _| {
            name.len() < 3
        }),
    ];
    let i = match tc {
        Some(tc) => tc.draw_silent(gs::integers::<usize>().max_value(filters.len() - 1)),
        None => 0,
    };
    let (src, pred) = &filters[i];
    fn filter(tree: &Tree, pred: &dyn Fn(&[u8], &Node) -> bool) -> Tree {
        tree.iter()
            .filter(|(name, node)| pred(name, node))
            .map(|(name, node)| match node {
                Node::Dir(sub) => (name.clone(), Node::Dir(filter(sub, pred))),
                other => (name.clone(), other.clone()),
            })
            .collect()
    }
    let filtered = Node::Dir(filter(tree, pred));
    check(
        &format!("builtins.path {{ path = {p}; name = \"tree\"; filter = p: t: {src}; }}"),
        Expect::value(source_path(&filtered, "tree")),
    );
    check(
        &format!("builtins.filterSource (p: t: {src}) {p}"),
        Expect::value(source_path(&filtered, "tree")),
    );
}

/// `fetchTree { type = "git"; }` and `fetchGit` on a repository with the
/// tree committed, then with a tracked file modified and an untracked one
/// added.
fn check_git(scratch: &Scratch, tree: &Tree, dirty: bool) {
    let repo = scratch.path("repo");
    write_tree(&repo, tree);
    run(Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .arg(&repo));
    run(Command::new("git").arg("-C").arg(&repo).args(["add", "-A"]));
    run(Command::new("git").arg("-C").arg(&repo).args([
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "m",
    ]));
    let rev = run(Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"]));
    let mut expected = without_empty_dirs(tree);
    let mut modified = false;
    if dirty {
        // Modify the first tracked file at the top level, if any, and add
        // an untracked one (which alone doesn't make the tree dirty).
        if let Some((name, Node::File { contents, .. })) = expected
            .iter_mut()
            .find(|(_, n)| matches!(n, Node::File { .. }))
        {
            contents.extend(b"dirty");
            std::fs::write(repo.join(OsStr::from_bytes(name)), &*contents).unwrap();
            modified = true;
        }
        std::fs::write(repo.join("nix-pbt-untracked"), "u").unwrap();
    }
    let node = Node::Dir(expected);
    let url = nix_string_literal(&format!("file://{}", repo.display()));
    let rev_value = if modified {
        format!("{rev}-dirty")
    } else {
        rev.clone()
    };
    check(
        &format!(
            "let r = builtins.fetchTree {{ type = \"git\"; url = {url}; }}; in \
             [ r.outPath r.narHash (r.rev or r.dirtyRev) r.lastModified ]"
        ),
        Expect::value(json!([
            source_path(&node, "source"),
            sri(&node),
            rev_value,
            MTIME
        ])),
    );
    // Lix also returns `revCount`, which CppNix only computes for
    // `fetchGit`.
    let ignored = if is_skipped("fetchtree-revcount") {
        "[ \"revCount\" ]"
    } else {
        "[ ]"
    };
    check(
        &format!(
            "builtins.attrNames (removeAttrs (builtins.fetchTree {{ type = \"git\"; url = {url}; }}) {ignored})"
        ),
        Expect::Unspecified,
    );
    check(
        &format!(
            "{WALK} (builtins.fetchTree {{ type = \"git\"; url = {url}; }}).outPath \"directory\""
        ),
        Expect::Value(walk_model(&node)),
    );
    check(
        &format!("(builtins.fetchGit {}).outPath", lit(&repo)),
        Expect::value(source_path(&node, "source")),
    );
    // With `rev`, the commit, not the working tree.
    check(
        &format!(
            "(builtins.fetchTree {{ type = \"git\"; url = {url}; rev = \"{rev}\"; }}).narHash"
        ),
        Expect::value(sri(&Node::Dir(without_empty_dirs(tree)))),
    );
}

/// How a tarball's entries are laid out.
#[derive(Clone, Copy, Debug)]
enum TarLayout {
    /// `tree/...`: one top-level directory, which fetchers strip.
    TopDir,
    /// `./...`.
    Dot,
    /// The top-level entries themselves (not for empty trees).
    Flat,
}

/// `fetchTree { type = "tarball"; }` and `fetchTarball` of the tree packed
/// with GNU tar.
fn check_tarball(scratch: &Scratch, tree: &Tree, layout: TarLayout, gzip: bool) {
    let src = scratch.path("tarsrc");
    write_tree(&src.join(""), &BTreeMap::new());
    write_tree(&src.join("tree"), tree);
    touch_all(&src);
    let file = scratch.path(if gzip { "t.tar.gz" } else { "t.tar" });
    let mut cmd = Command::new("tar");
    cmd.args([
        "--sort=name",
        "--owner=0",
        "--group=0",
        "--numeric-owner",
        "--format=gnu",
    ])
    .arg(if gzip { "-czf" } else { "-cf" })
    .arg(&file);
    match layout {
        TarLayout::TopDir => cmd.arg("-C").arg(&src).arg("tree"),
        TarLayout::Dot => cmd.arg("-C").arg(src.join("tree")).arg("."),
        TarLayout::Flat => cmd
            .arg("-C")
            .arg(src.join("tree"))
            .arg("--")
            .args(tree.keys().map(|n| OsStr::from_bytes(n))),
    };
    run(&mut cmd);

    // A single top-level directory is stripped.
    let top: Tree = match layout {
        TarLayout::TopDir => BTreeMap::from([(b"tree".to_vec(), Node::Dir(tree.clone()))]),
        _ => tree.clone(),
    };
    let unpacked = match top.iter().next() {
        Some((_, Node::Dir(sub))) if top.len() == 1 => sub.clone(),
        _ => top,
    };
    let node = Node::Dir(unpacked);
    let url = nix_string_literal(&format!("file://{}", file.display()));
    // fix reports `lastModified = 0`.
    let last_modified = if is_skipped("tarball-last-modified") {
        json!(null)
    } else {
        json!(MTIME)
    };
    let lm_field = if is_skipped("tarball-last-modified") {
        "null"
    } else {
        "(r.lastModified or null)"
    };
    check(
        &format!(
            "let r = builtins.fetchTree {{ type = \"tarball\"; url = {url}; }}; in \
             [ r.outPath r.narHash {lm_field} ]"
        ),
        Expect::value(json!([
            source_path(&node, "source"),
            sri(&node),
            last_modified
        ])),
    );
    check(
        &format!(
            "{WALK} (builtins.fetchTree {{ type = \"tarball\"; url = {url}; }}).outPath \"directory\""
        ),
        Expect::Value(walk_model(&node)),
    );
    check(
        &format!("builtins.fetchTarball {url}"),
        Expect::value(source_path(&node, "source")),
    );
}

// ---------------------------------------------------------------------------
// Properties

fn draw_tree(tc: &TestCase) -> Tree {
    let depth = tc.draw_silent(gs::integers::<usize>().max_value(2));
    let tree = Gen { tc }.tree(depth);
    let mut shown = String::new();
    show_tree(&tree, 2, &mut shown);
    tc.note(&format!("tree:\n{shown}"));
    tree
}

#[hegel::test]
fn path_copies(tc: TestCase) {
    let tree = draw_tree(&tc);
    let scratch = Scratch::new();
    let dir = scratch.path("tree");
    write_tree(&dir, &tree);
    touch_all(&dir);
    check_path_copies(Some(&tc), &dir, &tree);
}

#[hegel::test]
fn fetch_git(tc: TestCase) {
    let tree = draw_tree(&tc);
    let dirty = tc.draw(gs::booleans());
    // fix reports the dirty tree as `rev = HEAD`.
    if dirty {
        skip_known(&tc, "git-dirty-rev");
    }
    check_git(&Scratch::new().keep(), &tree, dirty);
}

/// CppNix serves a fetched git tree's store path from the repository it was
/// first fetched from: after that one is gone, fetching the same tree from
/// another repository gives a store path whose contents can't be read.
///
/// Not differential: CppNix doesn't copy fetched git trees to the store
/// eagerly, and another evaluator copying the same tree would hide the bug.
/// So each evaluator gets a tree of its own.
#[test]
fn stale_git_mount() {
    if is_skipped("git-stale-mount") {
        return;
    }
    let mut failures = Vec::new();
    for ev in evaluators() {
        let tree = BTreeMap::from([(
            b"sub".to_vec(),
            Node::Dir(BTreeMap::from([(
                b"f".to_vec(),
                Node::File {
                    contents: format!("{} {}", ev.name, std::process::id()).into_bytes(),
                    exec: false,
                },
            )])),
        )]);
        let (a, b) = (Scratch::new(), Scratch::new());
        for s in [&a, &b] {
            let repo = s.path("repo");
            write_tree(&repo, &tree);
            run(Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(&repo));
            run(Command::new("git").arg("-C").arg(&repo).args(["add", "-A"]));
            run(Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["commit", "-q", "-m", "m"]));
        }
        let fetch = |s: &Scratch| {
            format!(
                "(builtins.fetchTree {{ type = \"git\"; url = {}; }}).outPath",
                nix_string_literal(&format!("file://{}", s.path("repo").display()))
            )
        };
        // Only the top level: what was read through `a` stays cached.
        let first = ev.eval(&format!("builtins.readDir {}", fetch(&a)));
        drop(a);
        let second = ev.eval(&format!("builtins.readDir \"${{{}}}/sub\"", fetch(&b)));
        match (&first, &second) {
            (Outcome::Value(_), Outcome::Value(v)) if *v == json!({ "f": "regular" }) => {}
            _ => failures.push(format!(
                "{}:\n  first repository: {}\n  second repository, after deleting the first: {}",
                ev.name,
                first.describe(),
                second.describe()
            )),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[hegel::test]
fn fetch_tarball(tc: TestCase) {
    let tree = draw_tree(&tc);
    let mut layouts = vec![TarLayout::TopDir];
    // Lix wants exactly one top-level entry.
    if !is_skipped("tarball-top-level") {
        layouts.push(TarLayout::Dot);
        if !tree.is_empty() {
            layouts.push(TarLayout::Flat);
        }
    }
    let layout = tc.draw(gs::sampled_from(layouts).print_as_debug());
    let gzip = tc.draw(gs::booleans());
    check_tarball(&Scratch::new(), &tree, layout, gzip);
}

#[hegel::test]
fn fetchurl_file(tc: TestCase) {
    let contents = tc.draw(gs::vecs(gs::integers::<u8>()).max_size(100));
    let scratch = Scratch::new();
    let file = scratch.path("f");
    std::fs::write(&file, &contents).unwrap();
    let url = nix_string_literal(&format!("file://{}", file.display()));
    check(
        &format!("builtins.fetchurl {url}"),
        Expect::value(flat_path(&contents, "f")),
    );
    check(
        &format!("builtins.hashFile \"sha256\" (builtins.fetchurl {url})"),
        Expect::value(hex(&sha256(&contents))),
    );
}

/// The angryfiles tree: every one-byte name as a file, a directory, a
/// symlink, a dangling symlink and a symlink to itself, and every name
/// length as a file and a directory.
fn angry_tree() -> Tree {
    let one_byte: Vec<Vec<u8>> = (1..=255u8)
        .filter(|&b| b != b'/' && b != b'.')
        .map(|b| vec![b])
        .collect();
    let lengths: Vec<Vec<u8>> = (1..=255).map(|n| vec![b'x'; n]).collect();
    let file = || Node::File {
        contents: b"angry\n".to_vec(),
        exec: false,
    };
    let group = |names: &[Vec<u8>], f: &dyn Fn(&[u8]) -> Node| {
        Node::Dir(names.iter().map(|n| (n.clone(), f(n))).collect())
    };
    BTreeMap::from([
        (
            b"all_1_byte_file_names".to_vec(),
            group(&one_byte, &|_| file()),
        ),
        (
            b"all_1_byte_dir_names".to_vec(),
            group(&one_byte, &|_| {
                Node::Dir(BTreeMap::from([(b"f".to_vec(), file())]))
            }),
        ),
        (
            b"all_1_byte_symlink_names".to_vec(),
            group(&one_byte, &|_| {
                Node::Symlink(b"../all_1_byte_file_names/a".to_vec())
            }),
        ),
        (
            b"all_1_byte_broken_symlink_names".to_vec(),
            group(&one_byte, &|_| Node::Symlink(b"nonexistent".to_vec())),
        ),
        (
            b"all_1_byte_self_symlink_names".to_vec(),
            group(&one_byte, &|n| Node::Symlink(n.to_vec())),
        ),
        (
            b"all_length_file_names".to_vec(),
            group(&lengths, &|_| file()),
        ),
        (
            b"all_length_dir_names".to_vec(),
            group(&lengths, &|_| {
                Node::Dir(BTreeMap::from([(b"f".to_vec(), file())]))
            }),
        ),
    ])
}

#[test]
fn angry_corpus_path() {
    let tree = angry_tree();
    let scratch = Scratch::new();
    let dir = scratch.path("tree");
    write_tree(&dir, &tree);
    touch_all(&dir);
    check_path_copies(None, &dir, &tree);
}

#[test]
fn angry_corpus_git() {
    check_git(&Scratch::new().keep(), &angry_tree(), false);
}

#[test]
fn angry_corpus_tarball() {
    check_tarball(&Scratch::new(), &angry_tree(), TarLayout::TopDir, true);
}
