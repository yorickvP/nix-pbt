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
  (~100 lines). It's the most robust option and the easiest one to add to
  another evaluator.

Sessions are pooled (one per concurrently running test), restarted after a
crash or timeout, and recycled every 5000 evaluations. A REPL that exits
after printing an ordinary `error:` counts as that error, not as a crash
(Lix's REPL exits on stack overflows).

Throughput with `evaluators.env` (`nix-pbt-eval --bench`):

| evaluator | mode | evaluations/s |
|---|---|---|
| CppNix | server (C API) | ~54,000 |
| Lix | repl | ~16,000 |
| fix | repl, `--workers 1` | ~1,450 |
| fix | repl, default workers | ~350–500 |
| any | exec | ~30 |

A full run of the suite takes about a minute.

### Known-divergence tags

Hegel reports one (minimal) failure per property. To look past a known
divergence, tests mark the inputs that trigger it with a tag, and
`NIX_PBT_SKIP=tag1,tag2` rejects those inputs (or stops generating them):

| Tag | Test | Divergence |
|---|---|---|
| `circular-import` | `flakes::*` | fix accepts some circular flake imports |
| `lock-no-check` | `flakes::lock_files_are_interchangeable` | `fix flake lock` writes lock files with follow cycles / dangling follows |
| `empty-list-laziness` | `exprs::*` | fix forces `map`/`filter`/`sort`'s function on empty lists |
| `invalid-utf8-json` | `exprs::*` | Lix crashes printing invalid UTF-8 as JSON |

## Layout

- `src/eval.rs`: evaluator configuration, the three modes, session pool.
- `src/value.rs`: `NixValue`, rendering to Nix literals, JSON conversion,
  a model of `==`.
- `src/generators.rs`: Hegel generators for strings, attribute names, ints,
  floats, nested values.
- `src/store.rs`: exact models of store path computations: nix32,
  `compressHash`, `makeStorePath`, text paths, the ATerm `.drv` format,
  `hashDerivationModulo`, `placeholder`.
- `src/lib.rs`: `check`, `check_true`, `skip_known`, `commands`.
- `src/bin/nix-pbt-eval.rs`: evaluate or benchmark by hand.
- `servers/nix-capi/`: the CppNix evaluation server; `default.nix` builds it.

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
| `exprs.rs` | random well-typed expressions (let, lambdas, formals with defaults, `if`, `assert`, rec sets, `//`, `or`, `?`, `tryEval`, builtins) with `throw`s in lazy positions; laziness laws | |

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

Against CppNix 2.36.0pre20260925 (`../nix`), Lix 2.94.2 and fix 0.3.0.
"Model" means the behaviour of CppNix, which all models are written
against.

### fix

| Area | Input | Nix | fix |
|---|---|---|---|
| laziness | `map (throw "f") [ ]`, same for `filter`, `sort` | `[ ]` | error: forces the function |
| `toString` | `toString 3.002399751580331e16` | `"30023997515803312.000000"` | `"30023997515803310.000000"` |
| `splitVersion` | `"__"` / `"a_b"` / `"é"` | `["__"]` / `["a_b"]` / `["é"]` | `["_","_"]` / `["a","_","b"]` / type error |
| `compareVersions` | `"0" "2147483648"` | `1` (32-bit component quirk) | `-1` |
| `floor`/`ceil` | `floor 9.223372036854776e18` | error | `-9223372036854775808` |
| float literals | `1.1125369292536007e-308` | error: invalid float | accepted |
| `parseDrvName` | `"a-"` | `{ name = "a-"; }` | `{ name = "a"; }` |
| `toFile` | `toFile ".-" ""` | error: invalid store path name | accepted |
| `derivation` | `name` such that `name.drv` > 211 chars | error | accepted |
| `derivation` | `system = "\\"` | error (see Nix below) | accepted (same, unescaped, drv hash) |
| string context | `appendContext` for a path that doesn't exist | error | accepted |
| string context | `head (split "b" s)` | keeps `s`'s context | drops it |
| `convertHash` | `{ hash = "md5:…"; toHashFormat = …; }` | converts | error: missing attribute |
| `getFlake` | any `path:` flake whose lock is incomplete | doesn't write `flake.lock` | **writes `flake.lock` into the source tree** during evaluation |
| flakes | circular import via input overrides | error: circular import | evaluates |
| `flake lock` | `inputs.a.follows = "a"`, follows to a missing input | error (`LockFile::check`) | writes the lock file |
| `parseFlakeRef` | `"nixpkgs"`, `"flake:nixpkgs/branch"` | `{ type = "indirect"; id = "nixpkgs"; … }` | resolves through the registry to a `channels.nixos.org` tarball |
| `parseFlakeRef` | `"github:a/b#frag"`, `"/foo/bar#bla"` | error: unexpected fragment | fragment kept in `repo`/`path` |
| `parseFlakeRef` | `"github:foo/bar?xyzzy=1"`, `"/foo/bar?xyzzy=1"` | error: unknown parameter | ignored |
| `parseFlakeRef` | `"github://////owner%42/////repo%41///branch%43////"` | `github:ownerB/repoA/branchC` | `github:/` |
| `parseFlakeRef` | `"github:nixos/nix//master///something/"` | ref `master/something` | ref dropped |
| `parseFlakeRef` | `"git://somewhere/repo?ref=branch"` | `git://…` | `git+git://…` |
| `flakeRefToString` | `{ type = "path"; path = "/x y"; }`, `http://…/+3d.tar.gz` | percent-encodes (`%20`, `%2B`) | doesn't |
| `flakeRefToString` | `{ type = "indirect"; id = "a"; }` | `"flake:a"` | error: InvalidFlakeRef |

fix fails 21 of the 31 cases from Nix's own `flakeref.cc`.

What agrees: all other builtins covered here, over thousands of cases;
`drvPath`/`outPath` for random derivations, bit for bit; `follows` and
override resolution over thousands of random flake graphs; lock files,
which are identical to CppNix's and interchangeable with them; and 10,000
random expressions once the empty-list laziness issue is skipped.

### CppNix

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

- Printing a string that isn't valid UTF-8 as JSON **crashes** Lix (uncaught
  `nlohmann::json` exception, SIGABRT), in `nix-instantiate --json` and the
  REPL alike: `builtins.substring 0 1 "é"`. CppNix and fix report an error.
- With input overrides, Lix still follows an overridden `follows`: root
  overrides `f3`'s input `a` with a URL, `f3` declares `a.follows = "a"`.
  CppNix and fix use the override; Lix overflows the stack.
- `floor`/`ceil` of out-of-range floats return `-9223372036854775808`
  (CppNix: error).
- Rejects an output named `drv` (CppNix allows it).
- Fails 8 of the 31 `flakeref.cc` cases (mostly newer CppNix behaviour:
  percent-encoding, `%23` in refs, `revCount` on paths).
- Lix's error printer drops everything after a raw `ESC ]` in a quoted
  source line, newline included (it's treated as an OSC sequence).

## Ideas

- More ports: `fetchTree` on generated git repos (`libfetchers-tests`,
  `functional/fetchGit.sh`), `builtins.path`/`filterSource` with generated
  file trees (NAR hashing), relative `path:./sub` flake inputs, lock file
  *updates* (`nix flake update` of one input), `fromTOML`, `toXML`, regexes.
- Compare error *kinds* where implementations expose them.
- A `server` mode adapter for fix (its REPL works, but a protocol endpoint
  would be ~50 lines and 10× faster).
