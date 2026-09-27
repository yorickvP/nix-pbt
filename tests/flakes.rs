//! Flake input resolution: random graphs of local flakes with `follows`,
//! nested input overrides and non-flake inputs.
//!
//! A property-based take on `tests/functional/flakes/follow-paths.sh`,
//! `inputs.sh` and `non-flake-inputs.sh` from the Nix repo. The model below
//! mirrors `computeLocks` (src/libflake/flake.cc), `LockFile::check`
//! (src/libflake/lockfile.cc) and input resolution in `call-flake.nix`.
//!
//! Every flake `fI` has an output `value` that spells out the flakes its
//! inputs resolved to, e.g. `f0[a=f1[b=f2[]],c=f2]`, so evaluating the root's
//! `value` checks the whole resolved graph. Non-flake inputs contribute the
//! contents of their `data` file (`f2` above).

use hegel::TestCase;
use hegel::generators as gs;
use nix_pbt::*;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Input names are drawn from a small pool so that `follows` paths often
/// point somewhere real.
const NAMES: &[&str] = &["a", "b", "c"];

type InputPath = Vec<String>;

#[derive(Clone)]
enum Spec {
    /// `url = "path:<base>/fN"`
    Url(usize),
    /// `follows = "x/y"`
    Follows(InputPath),
}

#[derive(Clone)]
struct InputDecl {
    spec: Spec,
    /// `flake = false` when not set.
    flake: bool,
    /// `inputs.<name> = ...` overrides of this input's own inputs.
    overrides: BTreeMap<String, InputDecl>,
}

/// `flakes[0]` is the root.
#[derive(Clone)]
struct Graph {
    flakes: Vec<BTreeMap<String, InputDecl>>,
}

// ---------------------------------------------------------------------------
// Rendering

fn render_decl(d: &InputDecl, base: &str) -> String {
    let mut parts = Vec::new();
    match &d.spec {
        Spec::Url(j) => parts.push(format!("url = \"path:{base}/f{j}\";")),
        Spec::Follows(p) => parts.push(format!("follows = \"{}\";", p.join("/"))),
    }
    if !d.flake {
        parts.push("flake = false;".into());
    }
    if !d.overrides.is_empty() {
        let inner: Vec<String> = d
            .overrides
            .iter()
            .map(|(n, o)| format!("{n} = {};", render_decl(o, base)))
            .collect();
        parts.push(format!("inputs = {{ {} }};", inner.join(" ")));
    }
    format!("{{ {} }}", parts.join(" "))
}

fn render_flake(g: &Graph, i: usize, base: &str) -> String {
    let inputs: Vec<String> = g.flakes[i]
        .iter()
        .map(|(n, d)| format!("    {n} = {};\n", render_decl(d, base)))
        .collect();
    let names: Vec<String> = g.flakes[i].keys().map(|n| format!("\"{n}\"")).collect();
    format!(
        "{{\n  inputs = {{\n{}  }};\n  outputs = {{ self, ... }}@inputs:\n    let v = i: if i ? value then i.value else builtins.readFile \"${{i}}/data\";\n    in {{ value = \"f{i}[\" + builtins.concatStringsSep \",\" (map (n: n + \"=\" + v inputs.${{n}}) [ {} ]) + \"]\"; }};\n}}\n",
        inputs.concat(),
        names.join(" ")
    )
}

impl fmt::Debug for Graph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in 0..self.flakes.len() {
            writeln!(f, "# f{i}/flake.nix")?;
            write!(f, "{}", render_flake(self, i, "<base>"))?;
        }
        Ok(())
    }
}

hegel::pretty_print_as_debug!(Graph);

/// A directory with one subdirectory per flake, each containing `flake.nix`
/// and a `data` file. The flake directories are made read-only: fix writes
/// lock files during `builtins.getFlake`, which would leak into the other
/// evaluators' view of the flake. Removed on drop unless the test failed.
struct FlakeDir {
    base: PathBuf,
}

impl FlakeDir {
    fn create(g: &Graph) -> FlakeDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = tmp_dir().join(format!(
            "nix-pbt-flakes-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let base_s = base.to_str().unwrap().to_string();
        for i in 0..g.flakes.len() {
            let dir = base.join(format!("f{i}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("flake.nix"), render_flake(g, i, &base_s)).unwrap();
            std::fs::write(dir.join("data"), format!("f{i}")).unwrap();
        }
        let dir = FlakeDir { base };
        dir.set_readonly(true);
        dir
    }

    fn root(&self) -> PathBuf {
        self.base.join("f0")
    }

    fn set_readonly(&self, ro: bool) {
        for entry in std::fs::read_dir(&self.base).unwrap() {
            set_dir_readonly(&entry.unwrap().path(), ro);
        }
    }
}

fn set_dir_readonly(dir: &Path, ro: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if ro { 0o555 } else { 0o755 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

impl Drop for FlakeDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("flake graph kept at {}", self.base.display());
            return;
        }
        self.set_readonly(false);
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

// ---------------------------------------------------------------------------
// Generation

fn draw_path(tc: &TestCase) -> InputPath {
    let len = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    (0..len)
        .map(|_| tc.draw(gs::sampled_from(NAMES)).to_string())
        .collect()
}

/// An input declaration in flake `i` of `n`. Ordinary inputs only point to
/// later flakes (so the declared graph is acyclic); overrides may point
/// anywhere, which can create circular imports.
fn draw_decl(tc: &TestCase, n: usize, i: usize, depth: usize, is_override: bool) -> InputDecl {
    let targets: Vec<usize> = if is_override {
        (0..n).collect()
    } else {
        (i + 1..n).collect()
    };
    let spec = if !targets.is_empty() && tc.draw(gs::weighted_booleans(0.6)) {
        Spec::Url(tc.draw(gs::sampled_from(targets)))
    } else {
        Spec::Follows(draw_path(tc))
    };
    let flake = matches!(spec, Spec::Follows(_)) || !tc.draw(gs::weighted_booleans(0.2));
    let mut overrides = BTreeMap::new();
    if depth < 2 && matches!(spec, Spec::Url(_)) {
        let k = tc.draw(gs::integers::<usize>().max_value(2));
        for _ in 0..k {
            let name = tc.draw(gs::sampled_from(NAMES)).to_string();
            overrides.insert(name, draw_decl(tc, n, i, depth + 1, true));
        }
    }
    InputDecl {
        spec,
        flake,
        overrides,
    }
}

#[hegel::composite]
fn graphs(tc: &TestCase) -> Graph {
    let n = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
    let flakes = (0..n)
        .map(|i| {
            let k = tc.draw(gs::integers::<usize>().max_value(3));
            let mut inputs = BTreeMap::new();
            for _ in 0..k {
                let name = tc.draw(gs::sampled_from(NAMES)).to_string();
                inputs.insert(name, draw_decl(tc, n, i, 0, false));
            }
            inputs
        })
        .collect();
    Graph { flakes }
}

// ---------------------------------------------------------------------------
// Model

#[derive(Clone, Debug)]
enum Edge {
    Node(InputPath),
    Follows(InputPath),
}

#[derive(Debug)]
struct Node {
    dir: usize,
    is_flake: bool,
    inputs: BTreeMap<String, Edge>,
}

/// The lock graph: nodes keyed by their input path from the root.
type Lock = BTreeMap<InputPath, Node>;

struct Locker<'a> {
    g: &'a Graph,
    nodes: Lock,
    /// Overrides from ancestors: the first one registered (i.e. the one
    /// declared closest to the root) wins. Holds the declaration and the
    /// input path of the flake that declared it, which `follows` paths are
    /// relative to.
    overrides: BTreeMap<InputPath, (InputDecl, InputPath)>,
    /// Flakes being locked on the current path, for circular import
    /// detection. Like in Nix, the root itself is not included.
    parents: Vec<usize>,
}

fn join(p: &[String], s: &str) -> InputPath {
    let mut p = p.to_vec();
    p.push(s.to_string());
    p
}

impl Locker<'_> {
    fn add_overrides(&mut self, d: &InputDecl, prefix: &InputPath, declarer: &InputPath) {
        for (oid, od) in &d.overrides {
            let key = join(prefix, oid);
            self.overrides
                .entry(key.clone())
                .or_insert_with(|| (od.clone(), declarer.clone()));
            self.add_overrides(od, &key, declarer);
        }
    }

    fn compute(&mut self, dir: usize, path: &InputPath) -> Result<(), String> {
        let decls = &self.g.flakes[dir];
        for (id, d) in decls {
            self.add_overrides(d, &join(path, id), path);
        }
        for (id, original) in decls {
            let ipath = join(path, id);
            let (decl, declarer) = match self.overrides.get(&ipath) {
                Some((d, l)) => (d.clone(), l.clone()),
                None => (original.clone(), path.clone()),
            };
            // An override keeps the flakeness of the original input.
            let is_flake = original.flake;
            match decl.spec {
                Spec::Follows(t) => {
                    let mut target = declarer.clone();
                    target.extend(t);
                    self.nodes
                        .get_mut(path)
                        .unwrap()
                        .inputs
                        .insert(id.clone(), Edge::Follows(target));
                }
                Spec::Url(j) => {
                    self.nodes
                        .get_mut(path)
                        .unwrap()
                        .inputs
                        .insert(id.clone(), Edge::Node(ipath.clone()));
                    self.nodes.insert(
                        ipath.clone(),
                        Node {
                            dir: j,
                            is_flake,
                            inputs: BTreeMap::new(),
                        },
                    );
                    if is_flake {
                        if self.parents.contains(&j) {
                            return Err(format!("circular import of f{j}"));
                        }
                        self.parents.push(j);
                        self.compute(j, &ipath)?;
                        self.parents.pop();
                    }
                }
            }
        }
        Ok(())
    }
}

/// `doFind` from lockfile.cc, including its (shared) cycle detection.
fn find(
    nodes: &Lock,
    path: &InputPath,
    visited: &mut Vec<InputPath>,
) -> Result<Option<InputPath>, String> {
    if visited.contains(path) {
        return Err(format!("follow cycle at {}", path.join("/")));
    }
    visited.push(path.clone());
    let mut pos: InputPath = vec![];
    for elem in path {
        match nodes[&pos].inputs.get(elem) {
            Some(Edge::Node(p)) => pos = p.clone(),
            Some(Edge::Follows(t)) => match find(nodes, t, visited)? {
                Some(p) => pos = p,
                None => return Ok(None),
            },
            None => return Ok(None),
        }
    }
    Ok(Some(pos))
}

fn lock(g: &Graph) -> Result<Lock, String> {
    let mut locker = Locker {
        g,
        nodes: BTreeMap::new(),
        overrides: BTreeMap::new(),
        parents: Vec::new(),
    };
    locker.nodes.insert(
        vec![],
        Node {
            dir: 0,
            is_flake: true,
            inputs: BTreeMap::new(),
        },
    );
    locker.compute(0, &vec![])?;
    let nodes = locker.nodes;
    // LockFile::check: every `follows` must resolve.
    for node in nodes.values() {
        for edge in node.inputs.values() {
            if let Edge::Follows(t) = edge
                && !t.is_empty()
                && find(&nodes, t, &mut Vec::new())?.is_none()
            {
                return Err(format!("follows a non-existent input {}", t.join("/")));
            }
        }
    }
    Ok(nodes)
}

/// The `value` output of the node at `path`, following `call-flake.nix`.
fn value(nodes: &Lock, path: &InputPath, stack: &mut Vec<InputPath>) -> Result<String, String> {
    let node = &nodes[path];
    if !node.is_flake {
        return Ok(format!("f{}", node.dir));
    }
    if stack.contains(path) {
        return Err("infinite recursion".into());
    }
    stack.push(path.clone());
    let mut parts = Vec::new();
    for (id, edge) in &node.inputs {
        let target = match edge {
            Edge::Node(p) => p.clone(),
            Edge::Follows(t) => find(nodes, t, &mut Vec::new())?.expect("checked by lock()"),
        };
        parts.push(format!("{id}={}", value(nodes, &target, stack)?));
    }
    stack.pop();
    Ok(format!("f{}[{}]", node.dir, parts.join(",")))
}

fn model(g: &Graph) -> Result<String, String> {
    let nodes = lock(g)?;
    value(&nodes, &vec![], &mut Vec::new())
}

// ---------------------------------------------------------------------------
// Properties

/// Whether an override gives an input a URL where the flake it belongs to
/// declares it with `follows`. Lix still follows it (and overflows the
/// stack).
fn overrides_a_follows(g: &Graph) -> bool {
    fn check(g: &Graph, target: usize, overrides: &BTreeMap<String, InputDecl>) -> bool {
        overrides.iter().any(|(name, o)| {
            let declared = g.flakes[target].get(name);
            let replaces_follows = matches!(o.spec, Spec::Url(_))
                && matches!(declared, Some(d) if matches!(d.spec, Spec::Follows(_)));
            let nested = match (&o.spec, declared) {
                (Spec::Url(t), _) => check(g, *t, &o.overrides),
                (
                    _,
                    Some(InputDecl {
                        spec: Spec::Url(t), ..
                    }),
                ) => check(g, *t, &o.overrides),
                _ => false,
            };
            replaces_follows || nested
        })
    }
    g.flakes
        .iter()
        .flat_map(|f| f.values())
        .any(|d| match d.spec {
            Spec::Url(t) => check(g, t, &d.overrides),
            Spec::Follows(_) => false,
        })
}

#[hegel::test]
fn follows_resolution(tc: TestCase) {
    let g = tc.draw(graphs());
    let expect = model(&g);
    tc.note(&format!("model: {expect:?}"));
    tc.event(match &expect {
        Ok(_) => "model: ok".to_string(),
        Err(e) => format!(
            "model: {}",
            e.split(' ').take(2).collect::<Vec<_>>().join(" ")
        ),
    });
    tc.event(format!("flakes: {}", g.flakes.len()));
    // fix accepts some circular imports that Nix rejects.
    if matches!(&expect, Err(e) if e.starts_with("circular import")) {
        skip_known(&tc, "circular-import");
    }
    if overrides_a_follows(&g) {
        skip_known(&tc, "override-follows");
    }
    // After the skips: rejecting a test case unwinds, which keeps the
    // directory as if the test had failed.
    let dir = FlakeDir::create(&g);
    check(
        &format!(
            "(builtins.getFlake \"path:{}\").value",
            dir.root().display()
        ),
        expect,
    );
}

/// Lock files are interchangeable: for every locker in `NIX_PBT_FLAKE_LOCK`
/// (commands run in the root flake's directory, e.g. `nix flake lock`),
/// locking succeeds exactly when the model says the graph is valid, the
/// lock files agree, and every evaluator reads every locker's lock file to
/// the same resolved graph.
#[hegel::test]
fn lock_files_are_interchangeable(tc: TestCase) {
    let lockers = commands("NIX_PBT_FLAKE_LOCK");
    if lockers.is_empty() {
        return;
    }
    let g = tc.draw(graphs());
    let locked = lock(&g);
    let expect = model(&g);
    tc.note(&format!("model: {expect:?}"));
    if matches!(&expect, Err(e) if e.starts_with("circular import")) {
        skip_known(&tc, "circular-import");
    }
    // An input `path:<base>/f0` is the root flake's own directory, whose
    // hash changes when the lock file is written. CppNix and Lix accept the
    // lock file anyway; fix reports a NAR hash mismatch.
    fn refers_to_root(d: &InputDecl) -> bool {
        matches!(d.spec, Spec::Url(0)) || d.overrides.values().any(refers_to_root)
    }
    if g.flakes.iter().flat_map(|f| f.values()).any(refers_to_root) {
        skip_known(&tc, "root-input-hash");
    }
    // `fix flake lock` doesn't do LockFile::check: it writes lock files with
    // follow cycles and follows to non-existent inputs.
    if matches!(&expect, Err(e) if e.starts_with("follow")) {
        skip_known(&tc, "lock-no-check");
    }
    let dir = FlakeDir::create(&g);
    let root = dir.root();
    let lock_file = root.join("flake.lock");

    let mut locks: Vec<(String, serde_json::Value)> = Vec::new();
    for locker in &lockers {
        set_dir_readonly(&root, false);
        let _ = std::fs::remove_file(&lock_file);
        let (ok, stderr) = locker.run_in(&root);
        set_dir_readonly(&root, true);
        // Locking checks the graph; an infinite recursion only shows up
        // when evaluating it.
        match (&locked, ok) {
            (Ok(_), true) => {}
            (Err(_), false) => continue,
            // Lix follows a `follows` that an input override replaced, and
            // overflows the stack.
            (Ok(_), false)
                if stderr.contains("stack overflow") && is_skipped("override-follows") =>
            {
                tc.assume(false)
            }
            (Ok(_), false) => panic!(
                "{} failed to lock a valid graph (model: {expect:?}):\n{stderr}",
                locker.name
            ),
            (Err(e), true) => panic!("{} locked an invalid graph (model: {e})", locker.name),
        }
        // Nix doesn't write a lock file when there is nothing to lock; that
        // is compared like any other lock file content.
        let json = match std::fs::read_to_string(&lock_file) {
            Ok(text) => {
                tc.note(&format!("lock file by {}:\n{text}", locker.name));
                serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("{} wrote invalid JSON: {e}\n{text}", locker.name))
            }
            Err(_) => serde_json::Value::Null,
        };

        // Evaluate the flake using this lock file.
        check(
            &format!("(builtins.getFlake \"path:{}\").value", root.display()),
            expect.clone(),
        );
        locks.push((locker.name.clone(), json));
    }
    // Lix writes the declared ref as the `original` of an input that an
    // override replaced with a URL; CppNix and fix write the override.
    if is_skipped("override-original") {
        for (_, lock) in &mut locks {
            if let Some(nodes) = lock.get_mut("nodes").and_then(|n| n.as_object_mut()) {
                for node in nodes.values_mut() {
                    node.as_object_mut().map(|n| n.remove("original"));
                }
            }
        }
    }
    // fix names nodes differently when there are several of one input
    // (`a` and `a_2`); the lock files mean the same.
    if is_skipped("lock-node-names") {
        for (_, lock) in &mut locks {
            *lock = canonical_node_names(lock);
        }
    }
    for (name, lock) in &locks[1.min(locks.len())..] {
        assert_eq!(
            lock, &locks[0].1,
            "lock file by {name} differs from the one by {}",
            locks[0].0
        );
    }
}

/// A lock file with its nodes renamed `n0`, `n1`, ... in the order a
/// depth-first walk from the root, through inputs in name order, reaches
/// them. `follows` (arrays of input names) don't refer to node names.
fn canonical_node_names(lock: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Map, Value};
    let Some(nodes) = lock.get("nodes").and_then(Value::as_object) else {
        return lock.clone();
    };
    let root = lock["root"].as_str().unwrap_or("root").to_string();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(id) = stack.pop() {
        if names.contains_key(&id) {
            continue;
        }
        let new = if id == root {
            "root".to_string()
        } else {
            format!("n{}", names.len())
        };
        names.insert(id.clone(), new);
        if let Some(inputs) = nodes.get(&id).and_then(|n| n["inputs"].as_object()) {
            // Reversed, so that the stack pops them in name order.
            for target in inputs.values().rev().filter_map(Value::as_str) {
                stack.push(target.to_string());
            }
        }
    }
    let mut renamed = Map::new();
    for (id, node) in nodes {
        let mut node = node.clone();
        if let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) {
            for target in inputs.values_mut() {
                if let Some(t) = target.as_str() {
                    *target = Value::String(names.get(t).cloned().unwrap_or_else(|| t.to_string()));
                }
            }
        }
        renamed.insert(names.get(id).cloned().unwrap_or_else(|| id.clone()), node);
    }
    let mut lock = lock.clone();
    lock["nodes"] = Value::Object(renamed);
    lock["root"] = Value::String("root".into());
    lock
}

/// The examples from `follow-paths.sh`, as a sanity check of the model.
#[test]
fn model_matches_follow_paths_sh() {
    let url = |j| InputDecl {
        spec: Spec::Url(j),
        flake: true,
        overrides: BTreeMap::new(),
    };
    let follows = |p: &str| InputDecl {
        spec: Spec::Follows(p.split('/').map(String::from).collect()),
        flake: true,
        overrides: BTreeMap::new(),
    };
    // A: B (with foobar following A's foobar), foobar = E
    // B: foobar = E, goodoo follows C/goodoo, C (with foobar following B's foobar)
    // C: foobar = E, goodoo follows foobar
    let mut b = url(1);
    b.overrides.insert("foobar".into(), follows("foobar"));
    let mut c = url(2);
    c.overrides.insert("foobar".into(), follows("foobar"));
    let g = Graph {
        flakes: vec![
            BTreeMap::from([("B".into(), b), ("foobar".into(), url(3))]),
            BTreeMap::from([
                ("foobar".into(), url(3)),
                ("goodoo".into(), follows("C/goodoo")),
                ("C".into(), c),
            ]),
            BTreeMap::from([
                ("foobar".into(), url(3)),
                ("goodoo".into(), follows("foobar")),
            ]),
            BTreeMap::new(),
        ],
    };
    let nodes = lock(&g).unwrap();
    let edge = |p: &[&str], i: &str| {
        let p: InputPath = p.iter().map(|s| s.to_string()).collect();
        format!("{:?}", nodes[&p].inputs[i])
    };
    // The assertions from follow-paths.sh.
    assert_eq!(edge(&["B"], "C"), r#"Node(["B", "C"])"#);
    assert_eq!(edge(&["B"], "foobar"), r#"Follows(["foobar"])"#);
    assert_eq!(edge(&["B", "C"], "foobar"), r#"Follows(["B", "foobar"])"#);
    assert_eq!(
        model(&g).unwrap(),
        "f0[B=f1[C=f2[foobar=f3[],goodoo=f3[]],foobar=f3[],goodoo=f3[]],foobar=f3[]]"
    );
}
