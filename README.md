# nix-pbt

Property-based and differential tests for Nix evaluators, written with
[Hegel](https://hegel.dev/) ([hegel-rust](https://github.com/hegeldev/hegel-rust)).

The goal is to check alternative evaluators such as
[fix](https://github.com/psyclyx/fix) for equivalence with Nix, and to catch
bugs in Nix itself along the way.

## How it works

Each property draws inputs with Hegel, builds a Nix expression, and calls
`check(expr, expect)`. That evaluates the expression with **every
configured evaluator** and fails the test case if:

1. an evaluator crashed or timed out,
2. an evaluator disagrees with the first ("reference") one, or
3. the result differs from the Rust **model** of the builtin, if there is one.

Evaluation errors are compared by *whether* they happened, never by message.
The model can be `Expect::Value(json)`, `Expect::Error`, or
`Expect::Unspecified` (differential only). `check` returns the reference's
`Outcome`, so a property can build on it (e.g. check that a canonical form
is a fixed point).

Hegel shrinks failing inputs, so a report looks like this:

```
let expr = "(map (assert false; (v1: v1 + 0)) [ ])".to_string();
expression:
  (map (assert false; (v1: v1 + 0)) [ ])
model:
  <unspecified, differential only>
cppnix:
  []
lix:
  []
fix:
  <evaluation error> error: assertion failed
problems:
  fix disagrees with cppnix
```

## Running

```sh
# CppNix evaluation server (needs a Nix checkout with the C API; or omit
# --arg nixFlake to build against nixpkgs' Nix)
nix-build -A server-capi --arg nixFlake ../nix -o result-server-capi
# fix main with the server patch (--arg fixSrc ../fix for a checkout)
nix-build -A fix -o result-fix
# Lix main
nix build git+https://git.lix.systems/lix-project/lix -o result-lix

. ./evaluators.env              # CppNix (reference), Lix, fix; long-lived sessions
nix-shell --run 'cargo test --no-fail-fast'

HEGEL_TEST_CASES=2000 cargo test --test flakes   # more examples
cargo test --test strings substring              # one property
NIX_PBT_SKIP=circular-import cargo test --test flakes   # get past known divergences

# evaluate by hand with every configured evaluator
cargo run -q --bin nix-pbt-eval -- 'builtins.splitVersion "__"'
cargo run -q --bin nix-pbt-eval -- --bench 1000 '1 + 1'
```

Without any configuration, the tests use `nix-instantiate` from `$PATH`.
`evaluators-exec.env` is the same three-way setup in `exec` mode (no
server needed, ~100× slower).

| Variable | Meaning |
|---|---|
| `NIX_PBT_EVALUATORS` | `;`-separated `name=[mode:]command…` entries, see below. The first is the reference. |
| `NIX_PBT_FLAKE_LOCK` | `;`-separated `name=command…` entries that lock the flake in the current directory (for `flakes::lock_files_are_interchangeable`). |
| `NIX_PBT_SKIP` | Comma-separated known-divergence tags to skip, see below. |
| `NIX_PBT_TIMEOUT` | Per-evaluation timeout in seconds (default 10). |
| `NIX_PBT_MAX_SESSIONS` | At most this many sessions per evaluator (default: one per test thread). |
| `NIX_PBT_LOG` | Append every evaluation and each evaluator's outcome to this file, as JSON lines. Useful when Hegel reports a flaky test. |
| `NIX_PBT_TMPDIR` | Where generated flakes and file trees go (default `$TMPDIR`; nix-shell deletes that on exit, so set this to keep the trees of failing tests). |
| `HEGEL_TEST_CASES`, `HEGEL_SEED`, `HEGEL_STATISTICS`, … | Hegel settings, see the [hegel docs](https://docs.rs/hegeltest). |

### Evaluator modes

- **`exec`** (default): one process per expression, which is appended as
  the last argument. The command must print the strict value as JSON and
  exit non-zero on errors (`nix-instantiate --eval --strict --json -E`,
  `fix eval --strict --json -E`). About 30 evaluations/s.
- **`repl:`**: a long-lived REPL. Each expression is sent as
  `builtins.toJSON (EXPR)`, followed by `builtins.trace "NONCE" "NONCE"` to
  find the end of its output on stdout and stderr. Nix-style and
  JSON-style string literals and ANSI colours are all handled. Works with
  `nix repl` (CppNix, Lix) and `fix repl --json`.
- **`server:`**: a long-lived process speaking a tiny protocol:

  ```
  request:  <byte length>\n<expression>
  response: ok <byte length>\n<JSON value>
         |  error <byte length>\n<error message>
  ```

  `servers/nix-capi/server.c` implements it on the CppNix C API
  (~100 lines), and `servers/fix/pbt-server.patch` adds it to fix as
  `fix repl --pbt-server` (~60 lines of Zig; `default.nix` applies it).
  It's the most robust option and the easiest one to add to another
  evaluator: unlike a REPL it takes multi-line expressions and any byte.
  Both servers evaluate in read-write mode, like `nix repl`: sources and
  derivations are written to the store. The fix server uses one daemon
  connection instead of fix's usual pool of 8.

Sessions are pooled (one per concurrently running test, at most
`NIX_PBT_MAX_SESSIONS`), restarted after a crash or timeout, and recycled
every 5000 evaluations. A REPL that exits after printing an ordinary
`error:` counts as that error, not as a crash (Lix's REPL exits on stack
overflows). Lix ≥ 2.96 prints its prompt with `TERM=dumb` and applies
backspaces in piped input; the REPL driver strips prompts and sends `\b`
as an interpolation.

Throughput with `evaluators.env` (`nix-pbt-eval --bench`):

| evaluator | mode | evaluations/s |
|---|---|---|
| CppNix | server (C API) | ~60,000 |
| Lix | repl | ~30,000 |
| fix | server, `--workers 1` | ~45,000 |
| fix | server, default workers | ~21,000 |
| fix | repl, `--workers 1` | ~1,450 |
| any | exec | ~30 |

A full run of the suite takes under a minute.

### Known-divergence tags

Hegel reports one (minimal) failure per property. To look past a known
divergence, tests mark the inputs that trigger it with a tag, and
`NIX_PBT_SKIP=tag1,tag2` rejects those inputs (or stops generating them):

| Tag | Test | Divergence |
|---|---|---|
| `circular-import` | `flakes::*` | fix accepts some circular flake imports |
| `lock-no-check` | `flakes::lock_files_are_interchangeable` | `fix flake lock` writes lock files with follow cycles / dangling follows |
| `root-input-hash` | `flakes::lock_files_are_interchangeable` | fix: an input pointing at the root flake's own directory fails its lock file's `narHash` check |
| `override-original` | `flakes::lock_files_are_interchangeable` | Lix: the `original` of an input overridden with a URL is the declared ref, not the override |
| `override-follows` | `flakes::lock_files_are_interchangeable` | Lix follows a `follows` that an input override replaced (stack overflow) |
| `empty-list-laziness` | `exprs::*`, `builtins::*` | fix doesn't force `map`/`filter`/`sort`/`all`/…'s function on empty lists |
| `invalid-utf8-json` | `exprs::*` | Lix < 2.95 crashes printing invalid UTF-8 as JSON |
| `convert-hash` | `store::convert_hash`, `builtins::builtin_names` | Lix and fix have no `convertHash` |
| `error-order` | `builtins::*`, `exprs::error_kinds_agree` | which of two errors wins depends on evaluation order, which differs (see Findings); only plant `throw`s in callbacks |
| `context-compare` | `builtins::*` | fix: `<` on a string with context and one without is a type error |
| `no-context` | `builtins::*` | fix accepts string context where CppNix rejects it |
| `lax-types` | `builtins::*` | fix doesn't type-check some arguments like CppNix |
| `function-equality` | `builtins::*` | Lix ≥ 2.94 and fix: `f == f` is `true`; CppNix: `false` |
| `fetchtree-revcount` | `fetchers::*git*` | Lix returns `revCount` from `fetchTree` (CppNix only from `fetchGit`) |
| `tarball-top-level` | `fetchers::fetch_tarball` | Lix wants exactly one top-level entry in a tarball |
| `fetch-path-name` | `fetchers::*path*` | fix: `fetchTree { type = "path"; }` is named after the directory, not `source` |
| `tarball-last-modified` | `fetchers::*tarball*` | fix: tarballs have `lastModified = 0` |
| `git-dirty-rev` | `fetchers::fetch_git` | fix: a dirty repository gets `rev = HEAD` |
| `git-stale-mount` | `fetchers::stale_git_mount` | CppNix reads a fetched git tree from a deleted repository |
| `store-write-desync` | `store::store_errors_dont_poison_the_session`, and the harness | fix: after the daemon rejects a write, the connection is out of sync. With the tag, the harness also restarts a session after a daemon error, so that it doesn't spill into other tests |
| `versions` | `builtins::*` | fix: `splitVersion`/`compareVersions` (see `versions.rs`, which still reports them) |
| `failed-copy-crash` | `store::store_errors_dont_poison_the_session` | Lix main segfaults on the first store copy after a failed one |

`known.env` sets all of them (for CppNix main, Lix main and fix main):

```sh
. ./evaluators.env; . ./known.env
nix-shell --run 'cargo test --no-fail-fast'
```

Divergences found before tags existed (e.g. `splitVersion "__"`) still fail
their tests; see Findings.

## Layout

- `src/eval.rs`: evaluator configuration, the three modes, session pool.
- `src/value.rs`: `NixValue`, rendering to Nix literals, JSON conversion,
  a model of `==`.
- `src/generators.rs`: Hegel generators for strings, attribute names, ints,
  floats, nested values.
- `src/store.rs`: exact models of store path computations: nix32,
  `compressHash`, `makeStorePath`, text paths, the ATerm `.drv` format,
  `hashDerivationModulo`, `placeholder`.
- `src/lib.rs`: `check`, `check_true`, `skip_known`, `commands`, `tmp_dir`.
- `src/bin/nix-pbt-eval.rs`: evaluate or benchmark by hand.
- `servers/nix-capi/`: the CppNix evaluation server; `default.nix` builds it.
- `servers/fix/pbt-server.patch`: `fix repl --pbt-server`; `default.nix`
  builds fix main (or `--arg fixSrc`) with it.

| Test file | What | Ported from (Nix repo) |
|---|---|---|
| `strings.rs` | `stringLength`, `substring`, `concatStringsSep`, `replaceStrings`, `toString`, `typeOf`, literals, interpolation, invalid UTF-8 | |
| `lists.rs` | `length`, `elemAt`, `head`/`tail`, `sort` (incl. stability), `genList`, `concatLists`, `filter`, `elem`, `foldl'`, `==` | |
| `attrs.rs` | `attrNames`, `attrValues`, `hasAttr`/`getAttr`, `removeAttrs`, `intersectAttrs`, `//`, `listToAttrs`, `mapAttrs`, `catAttrs` | |
| `arith.rs` | integer ops and overflow, bit ops, float arithmetic, mixed comparisons, `floor`/`ceil`, literals | |
| `json.rs` | `toJSON`/`fromJSON` | |
| `versions.rs` | `splitVersion`, `compareVersions` | `libstore/names.cc` |
| `flakes.rs` | random flake graphs with `follows`, nested overrides, `flake = false`: resolved graph vs a model of `computeLocks`; lock files from each implementation read by every other one | `functional/flakes/follow-paths.sh`, `inputs.sh`, `non-flake-inputs.sh` |
| `flakeref.rs` | `parseFlakeRef`/`flakeRefToString`: the unit test table, attrs round trip, canonical form is a fixed point, parsing URL-ish strings | `libflake-tests/flakeref.cc` |
| `store.rs` | `toFile`, `placeholder`, `derivation` (exact `drvPath`/`outPath`, context), `appendContext`/`getContext` round trip, context propagation, `parseDrvName`, `hashString`, `convertHash` | `libexpr-tests/value/context.cc`, `libstore/derivations`, `names.cc` |
| `exprs.rs` | random well-typed expressions (let, lambdas, formals with defaults, `if`, `assert`, rec sets, `//`, `or`, `?`, `tryEval`, builtins) with `throw`s in lazy positions; laziness laws; error kinds (`tryEval (deepSeq e true)`) | |
| `builtins.rs` | every builtin and operator applied to arbitrary arguments (mostly of the right type, with `throw`, failed assertions, missing `<paths>`, `abort` and type errors planted in them) inside `tryEval (deepSeq e true)`, which tells catchable errors from uncatchable ones; string context rejection; the set of builtins | |
| `fetchers.rs` | generated "angry" file trees (every kind of byte in names, invalid UTF-8, 255-byte names, symlinks, executable bits, empty directories) through `builtins.path` (+ `filter`), `filterSource`, `fetchTree` (`path`, `git` clean and dirty, `tarball` with three layouts), `fetchTarball`, `fetchurl`, and a `readDir`/`hashFile` walk, against a model of NAR hashes and store paths; the [angryfiles](https://github.com/jakeogh/angryfiles) corpus (every one-byte name, every name length) | `libfetchers-tests`, `functional/fetchGit.sh`, `tarball.sh` |

### Writing a property

```rust
#[hegel::test]
fn string_length(tc: TestCase) {
    let s = tc.draw(nix_strings());
    check(
        &format!("builtins.stringLength {}", nix_string_literal(&s)),
        Expect::value(s.len()),
    );
}
```

Build expressions with `nix_string_literal` / `nix_int_literal` /
`nix_float_literal` / `NixValue::to_nix`: they handle `${`, `\r`, negative
numbers, `i64::MIN`, and floats (`1e3` isn't a float literal in Nix).
Expressions must be single-line for `repl` mode.

Some tests write to the Nix store: `toFile`, `derivation`, and the flake
tests (which copy the generated flakes into the store). The generated flake
trees live in `$TMPDIR/nix-pbt-flakes-*`, are made read-only (fix writes
lock files during `getFlake` otherwise), and are kept when a test fails.

## Findings

Against CppNix 2.36.0pre20260925 (`../nix`), Lix 2.96.0-dev (main,
`0c16765`) and fix 0.3.0 (main, `3b2ffbb2`, unchanged since 2026-09-18).
"Model" means the behaviour of CppNix, which all models are written
against.

### fix

| Area | Input | Nix | fix |
|---|---|---|---|
| laziness | `map (throw "f") [ ]`, same for `filter`, `sort`, `all`, `any`; `sort` of one element, `filterSource` of a file | forces the function | doesn't |
| laziness | `removeAttrs { } [ (throw "t") ]` | error | `{ }` |
| `toString` | `toString 3.002399751580331e16` | `"30023997515803312.000000"` | `"30023997515803310.000000"` |
| `splitVersion` | `"__"` / `"a_b"` / `"é"` | `["__"]` / `["a_b"]` / `["é"]` | `["_","_"]` / `["a","_","b"]` / type error |
| `compareVersions` | `"0" "2147483648"` | `1` (32-bit component quirk) | `-1` |
| `floor`/`ceil` | `floor 9.223372036854776e18` | error | `-9223372036854775808` |
| float literals | `1.1125369292536007e-308` | error: invalid float | accepted |
| `parseDrvName` | `"a-"` | `{ name = "a-"; }` | `{ name = "a"; }` |
| `toFile` | `toFile ".-" ""` | error: invalid store path name | accepted without `--read-write-mode` (with it, the daemon rejects it) |
| `derivation` | `name` such that `name.drv` > 211 chars | error | accepted |
| `derivation` | `system = "\\"` | error (see Nix below) | accepted (same, unescaped, drv hash) |
| string context | `appendContext` for a path that doesn't exist | error | accepted |
| string context | `head (split "b" s)` | keeps `s`'s context | drops it |
| string context | `"${drv}" < "z"`, `lessThan`, `sort lessThan`, `[ s ] < [ "z" ]`: one string with context, one without | compares | **error: type error** |
| string context | `getEnv`, `placeholder`, `match`, `split`, `parseDrvName`, `compareVersions`, `splitVersion`, `removeAttrs`, `listToAttrs`, `catAttrs`, `groupBy`, `unsafeGetAttrPos`, `derivation`'s `outputs`/`outputHash`/`outputHashMode`, of a string with context | error | accepted |
| types | `storePath "/nonexistent"`, `storePath { outPath = "/x"; }` | error | returns it |
| types | `appendContext ./p { … }`, `appendContext "a" { a = …; }`, `hasContext ./p`, `toFile "a" ./p`, `toFile "a" { __toString = …; }`, `removeAttrs { } [ 1 ]` | error | accepted |
| types | `toPath { outPath = "/x"; }`, `unsafeDiscardOutputDependency { __toString = …; }` | coerces | error |
| types | `unsafeDiscardOutputDependency ./p` | copies `./p` to the store | returns the path |
| `convertHash` | | converts | doesn't exist (neither in Lix) |
| `fetchTree`, flakes | `outPath` of a fetched tree or flake (also `self`), in `fix eval` without `--read-write-mode` | a store path, with context | the source directory or `~/.cache/fix/…/source`, without context: **`drvPath`s of derivations using them (`src = self;`) differ from Nix's** |
| `fetchTree` git | a dirty working tree | `dirtyRev`, no `rev` (`fetchGit`: `rev = "000…"`, `revCount = 0`) | **`rev` = HEAD with the dirty tree's `narHash`**: fetching that `rev` gives another `narHash` |
| `fetchTree` path | store path name | `…-source` | named after the directory |
| `fetchTree` tarball | `lastModified` | newest mtime in the tarball | `0` |
| `==` | `let f = x: x; in f == f`, `builtins.add == builtins.add` | `false` | `true`, like Lix ≥ 2.94 |
| store writes | after the daemon rejects a write (`toFile "..-x" ""`), in the same session | later writes work | **the connection is out of sync for good**: the next write gets the earlier write's error, then `ReadFailed`, then `WriteFailed`. With the default pool of 8 connections this looks random |
| `derivation` | `system = ""` | error: required attribute `system` missing | accepted |
| `getFlake` | any `path:` flake whose lock is incomplete | doesn't write `flake.lock` | **writes `flake.lock` into the source tree** during evaluation |
| flakes | circular import via input overrides | error: circular import | evaluates |
| flakes | an input `path:/…/f0` in `/…/f0/flake.nix` (the flake's own directory), locked by CppNix | uses the lock file | error: NAR hash mismatch (the lock file changed the directory) |
| `flake lock` | `inputs.a.follows = "a"`, follows to a missing input | error (`LockFile::check`) | writes the lock file |
| `parseFlakeRef` | `"nixpkgs"`, `"flake:nixpkgs/branch"` | `{ type = "indirect"; id = "nixpkgs"; … }` | resolves through the registry to a `channels.nixos.org` tarball |
| `parseFlakeRef` | `"github:a/b#frag"`, `"/foo/bar#bla"`, `"git+https://#"` | error: unexpected fragment / drops `#` | fragment kept in `repo`/`path`/`url` |
| `parseFlakeRef` | `"github:foo/bar?xyzzy=1"`, `"/foo/bar?xyzzy=1"` | error: unknown parameter | ignored |
| `parseFlakeRef` | `"github://////owner%42/////repo%41///branch%43////"` | `github:ownerB/repoA/branchC` | `github:/` |
| `parseFlakeRef` | `"github:nixos/nix//master///something/"` | ref `master/something` | ref dropped |
| `parseFlakeRef` | `"git://somewhere/repo?ref=branch"` | `git://…` | `git+git://…` |
| `flakeRefToString` | `{ type = "path"; path = "/x y"; }`, `http://…/+3d.tar.gz` | percent-encodes (`%20`, `%2B`) | doesn't |
| `flakeRefToString` | `{ type = "indirect"; id = "a"; }` | `"flake:a"` | error: InvalidFlakeRef |

fix fails 21 of the 31 cases from Nix's own `flakeref.cc`.

**`tryEval`.** I looked for `throw`s that fix's `tryEval` doesn't catch
but Nix's does: in the callbacks of every higher-order builtin, in
`deepSeq`, `toJSON`, `toXML`, derivation attributes, string coercion
(`__toString`), `<path>` lookups, `import`, with `--workers 1` and the
parallel evaluator, over tens of thousands of generated cases
(`builtins.rs`). There were none. Where the kinds of error differ, fix
catches a `throw` that Nix never gets to, because Nix fails on an earlier
argument first with an error `tryEval` doesn't catch:

| Expression | Nix | fix |
|---|---|---|
| `null + (throw "t")` | coerces `null` to a string first: type error | forces both operands: `throw` |
| `substring (-1) (throw "t") ""` | negative start position | `throw` |
| `hashFile "" (throw "t")` | unknown hash algorithm | `throw` |
| `findFile [ ] "${ctx}"` | string has context | not found (a `throw`) |
| `addErrorContext { } (throw "t")` | the `throw`, then coercing the context message fails, which replaces it | `throw` (the message is never coerced) |

What agrees: all other builtins covered here, over thousands of cases;
`drvPath`/`outPath` for random derivations, bit for bit; `follows` and
override resolution over thousands of random flake graphs; lock files,
which are identical to CppNix's and interchangeable with them; 10,000
random expressions once the empty-list laziness issue is skipped; and NAR
hashes and store paths of `builtins.path`, `filterSource` and `fetchTree`
(`git` and `tarball`, read-write mode) for thousands of angry file trees
and the angryfiles corpus.

### CppNix

- **Fetched git trees are read from a stale repository.** CppNix doesn't
  copy a `fetchTree` git result to the store; it serves the store path
  from the repository it fetched it from. If the same tree (same NAR hash)
  is later fetched from another repository after the first one is gone,
  its contents are unreadable:

  ```
  nix-repl> builtins.readDir (builtins.fetchTree { type = "git"; url = "file:///tmp/a"; }).outPath
  { sub = "directory"; }
  # rm -rf /tmp/a; /tmp/b is a clone of it
  nix-repl> builtins.readDir "${(builtins.fetchTree { type = "git"; url = "file:///tmp/b"; }).outPath}/sub"
  error: path '/nix/store/7paqfyacpbg57ax0vban7cm25rgkv792-source/sub' does not exist
  ```

  (`fetchers::stale_git_mount`.) Another evaluator copying the same tree
  to the store hides the bug, which made this one show up as a flaky test.
- `a // b` evaluates `b` first since 2.32 (97ce7759d, "Use same naive
  iterative merging but with `evalForUpdate`"); 2.3 to 2.31 evaluate `a`
  first. Evaluation order is unspecified, but it's visible:
  `tryEval ((abort "a") // (throw "t"))` is caught by CppNix and aborts in
  Lix and fix.
- `fetchTree { type = "git"; }` doesn't return `revCount` any more
  (`fetchGit` does); Lix still does.
- `derivation { system = "\\"; … }` (or a `"` in `system`) writes the
  platform into the `.drv` unescaped (`printUnquotedString` in
  `derivation/aterm.cc`, meant for restricted alphabets), then fails to
  parse its own `.drv`: `error parsing derivation '…': expected string ','`.
  `system` should probably be validated when the derivation is created.
  Lix has the same bug.
- `compareVersions` parses components with `string2Int<int>`, so a component
  ≥ 2³¹ counts as a *word* and sorts before every number:
  `compareVersions "0" "2147483648" == 1`.
- Subnormal float literals are rejected (`invalid float '5.0e-324'`).

### Lix

On main (2.96.0-dev):

- **Segfault after a failed store copy.** Once copying a path that
  doesn't exist to the store has failed, the next copy crashes in
  `RemoteStore::addCAToStore`:

  ```
  nix-repl> "${/nonexistent}"
  error: … No such file or directory
  nix-repl> "${./file}"
  Segmentation fault (core dumped)
  ```

  Same after `fetchTree { type = "path"; path = "/nonexistent"; }` or
  `getFlake "path:/nonexistent"`. A regression: 2.94.2 and 2.95.2 are
  fine. Anything that keeps evaluating after an error with the same store
  connection (the REPL, `nix-eval-jobs`) is affected
  (`store::store_errors_dont_poison_the_session`).
- `builtins.toXML` of a cyclic value (`let x = { a = x; }; in x`) never
  finishes (2.95.2 too); CppNix and fix report a stack overflow.
- The REPL prints its prompt to stdout when `TERM=dumb`, even when stdin
  isn't a terminal, applies backspaces in piped input
  (`"a<BS>b"` is read as `"b"`), and colours values despite `NO_COLOR`.
- `0 ++ (throw "t")` checks the left operand's type before forcing the
  right one: an uncatchable type error, where CppNix and fix evaluate both
  and `tryEval` catches the `throw`.
- Since 2.94, functions compare equal to themselves: `f == f`,
  `builtins.add == builtins.add` and `let s.f = f; in s.f == s.f` are
  `true`. This is deliberate
  ([cl/4556](https://gerrit.lix.systems/c/lix/+/4556), "Function equality
  semantics are more consistent, but still bad"). CppNix (2.3 to main) and
  Lix ≤ 2.93 say `false`, except where `==` on lists and sets compares
  elements by pointer first (`[ f ] == [ f ]` is `true`).
- Tarballs must have exactly one top-level entry ("contains an unexpected
  number of top-level files"); CppNix unpacks the rest as they are.
- `fetchTree { type = "git"; }` returns `revCount`; no `convertHash`.
- Rejects an output named `drv` (CppNix allows it).
- Fails several of the 31 `flakeref.cc` cases (mostly newer CppNix
  behaviour: percent-encoding, `%23` in refs, `revCount` on paths).

- Lock files: the `original` of an input that an override replaced with
  a URL is the input's declared ref (2.94 to main), where CppNix writes
  the override: `inputs.a = { url = "path:/f1"; inputs.a.url = "path:/f1"; }`
  with `f1` declaring `inputs.a.url = "path:/f2"` gives a node with
  `locked` `/f1` and `original` `/f2`.
- With input overrides, Lix still follows an overridden `follows`: the
  root overrides `f1`'s input `a` with a URL, `f1` declares
  `a.follows = "a"`. CppNix and fix use the override; Lix overflows the
  stack, in `nix flake lock` too.

Fixed since 2.94.2: printing invalid UTF-8 as JSON crashed Lix (fixed in
2.95); `floor`/`ceil` of out-of-range floats returned
`-9223372036854775808` (fixed on main). Not re-checked on main: the error
printer dropping everything after a raw `ESC ]` in a quoted source line.

### Daemons

`with-daemon.sh DAEMON -- CMD` runs `CMD` against a private daemon of any
version, without root: in user and mount namespaces, with a fresh
`/nix/store` that holds only the closures the command needs. With it:

- The whole suite gives the same results with private Lix 2.95.2, Lix main
  and CppNix main daemons as with the host's Lix 2.95.2 daemon. None of the
  findings depend on the daemon version.
- The Lix main segfault, fix's out-of-sync connection and CppNix's stale git
  mount happen with each of those daemons: they're client bugs. The first two look like the
  same bug: after a streaming store operation fails, the client keeps using
  the connection (CppNix clients drop it).
- In read-write mode, fix uses a pool of 8 daemon connections per process
  (`default_pool_workers` in `store/daemon/runtime.zig`), independent of
  `--workers`; `fix repl --pbt-server` opened all 8 at startup, before any
  request. A Lix
  daemon runs ~18 threads per connection (CppNix ~2), so 13 fix processes
  make a Lix daemon use ~1,900 threads. On the host daemon here that ran
  into a task limit: new connections were reset, and the daemon's workers
  crashed (`Lix crashed. This is a bug … std::system_error: Resource
  temporarily unavailable` instead of an error). The same happens with a
  private daemon given a low `RLIMIT_NPROC`. Curiously only Lix main clients
  failed; Lix 2.95 and CppNix clients got through. The fix server now uses
  one connection, which avoids all this.

## Ideas

- A `server` for Lix (on its C++ API): its REPL is the least robust
  session type.
- More ports: relative `path:./sub` flake inputs, lock file *updates*
  (`nix flake update` of one input), `fromTOML`, `toXML`, regexes.
- Angrier tarballs (hard links, `..` and absolute entries, pax headers,
  duplicate entries) with the `tar` crate; git submodules, `ref`/`rev`
  combinations, `.gitattributes` `export-ignore`.
- Compare error *messages* where implementations agree on them.
