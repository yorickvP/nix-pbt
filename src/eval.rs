//! Running expressions through external Nix evaluators.
//!
//! Evaluators are configured with `NIX_PBT_EVALUATORS`, a `;`-separated list
//! of `name=[mode:]command args...` entries. The first entry is the reference
//! that the others are compared against. For example:
//!
//! ```text
//! NIX_PBT_EVALUATORS='lix=repl:nix repl;cppnix=server:nix-pbt-server-capi;fix=repl:fix repl --json'
//! ```
//!
//! There are three modes:
//!
//! - `exec` (the default): one process per expression. The expression is
//!   appended as the last argument; the command must print the strict value
//!   as JSON and exit non-zero on evaluation errors
//!   (`nix-instantiate --eval --strict --json -E`).
//!
//! - `repl`: a long-lived REPL, fed one line per expression. Each expression
//!   is sent as `builtins.toJSON (EXPR)`, followed by
//!   `builtins.trace "NONCE" "NONCE"` to find the end of its output on both
//!   stdout and stderr. The printed value may be a Nix or a JSON string
//!   literal, and ANSI colours are ignored, so this works with `nix repl`
//!   (CppNix and Lix) and `fix repl --json` alike.
//!
//! - `server`: a long-lived process speaking the nix-pbt server protocol:
//!
//!   ```text
//!   request:  <byte length>\n<expression>
//!   response: ok <byte length>\n<JSON value>
//!          |  error <byte length>\n<error message>
//!   ```
//!
//!   See `servers/nix-capi` for an implementation on the CppNix C API.
//!
//! Sessions of the long-lived modes are pooled per evaluator (one per
//! concurrently running test, at most `NIX_PBT_MAX_SESSIONS`) and restarted
//! after a crash, a timeout, or [`RECYCLE_AFTER`] evaluations.

use serde_json::Value as Json;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const DEFAULT_EVALUATORS: &str = "nix=nix-instantiate --eval --strict --json -E";

/// Restart long-lived sessions after this many evaluations, to bound any
/// state (memory, caches) that builds up in them.
pub const RECYCLE_AFTER: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Exec,
    Repl,
    Server,
}

pub struct Evaluator {
    pub name: String,
    pub mode: Mode,
    pub argv: Vec<String>,
    pool: Mutex<Pool>,
    returned: Condvar,
}

/// Idle sessions, and how many exist in total (idle or in use).
struct Pool {
    idle: Vec<Session>,
    live: usize,
}

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluator")
            .field("name", &self.name)
            .field("mode", &self.mode)
            .field("argv", &self.argv)
            .finish()
    }
}

impl Evaluator {
    /// Parse one `name=[mode:]command...` entry.
    pub fn parse(entry: &str) -> Evaluator {
        let (name, cmd) = entry
            .split_once('=')
            .unwrap_or_else(|| panic!("evaluator entry {entry:?} is not name=command"));
        let cmd = cmd.trim_start();
        let (mode, cmd) = [
            ("exec:", Mode::Exec),
            ("repl:", Mode::Repl),
            ("server:", Mode::Server),
        ]
        .iter()
        .find_map(|(prefix, mode)| cmd.strip_prefix(prefix).map(|rest| (*mode, rest)))
        .unwrap_or((Mode::Exec, cmd));
        let argv = shell_words::split(cmd)
            .unwrap_or_else(|e| panic!("bad command for evaluator {name}: {e}"));
        assert!(!argv.is_empty(), "empty command for evaluator {name}");
        Evaluator {
            name: name.trim().to_string(),
            mode,
            argv,
            pool: Mutex::new(Pool {
                idle: Vec::new(),
                live: 0,
            }),
            returned: Condvar::new(),
        }
    }

    /// Evaluate one expression.
    pub fn eval(&self, expr: &str) -> Outcome {
        if self.mode == Mode::Exec {
            return exec_once(self, expr);
        }
        let mut session = self.checkout();
        let out = session.eval(self.mode, expr);
        // fix's daemon connection gets out of sync after the daemon rejects
        // a write (see `store::store_errors_dont_poison_the_session`); start
        // afresh rather than let that spill into other tests.
        if matches!(&out, Outcome::Error(e) if e.contains("error: daemon:"))
            && crate::is_skipped("store-write-desync")
        {
            session.healthy = false;
        }
        let mut pool = self.pool.lock().unwrap();
        if session.healthy && session.evals < RECYCLE_AFTER {
            pool.idle.push(session);
        } else {
            pool.live -= 1;
        }
        self.returned.notify_one();
        out
    }

    /// Evaluate `exprs` one after the other in a new session of its own, for
    /// properties about state that carries over between evaluations.
    pub fn eval_in_new_session(&self, exprs: &[&str]) -> Vec<Outcome> {
        if self.mode == Mode::Exec {
            return exprs.iter().map(|e| exec_once(self, e)).collect();
        }
        let mut session = Session::spawn(self);
        exprs.iter().map(|e| session.eval(self.mode, e)).collect()
    }

    /// An idle session, a new one, or (at `NIX_PBT_MAX_SESSIONS`) the next
    /// one to be returned.
    fn checkout(&self) -> Session {
        let mut pool = self.pool.lock().unwrap();
        loop {
            if let Some(s) = pool.idle.pop() {
                return s;
            }
            if pool.live < max_sessions() {
                pool.live += 1;
                drop(pool);
                return Session::spawn(self);
            }
            pool = self.returned.wait(pool).unwrap();
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.argv[0]);
        cmd.args(&self.argv[1..])
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }
}

/// The configured evaluators, parsed once per test binary.
pub fn evaluators() -> &'static [Evaluator] {
    static EVS: OnceLock<Vec<Evaluator>> = OnceLock::new();
    EVS.get_or_init(|| {
        let spec = std::env::var("NIX_PBT_EVALUATORS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_EVALUATORS.to_string());
        let evs: Vec<Evaluator> = spec
            .split(';')
            .filter(|e| !e.trim().is_empty())
            .map(Evaluator::parse)
            .collect();
        assert!(!evs.is_empty(), "NIX_PBT_EVALUATORS lists no evaluators");
        evs
    })
}

/// `NIX_PBT_MAX_SESSIONS`: how many sessions of one evaluator may run at
/// once (default: unlimited, i.e. one per test thread).
fn max_sessions() -> usize {
    static MAX: OnceLock<usize> = OnceLock::new();
    *MAX.get_or_init(|| {
        std::env::var("NIX_PBT_MAX_SESSIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX)
    })
}

fn timeout() -> Duration {
    let secs = std::env::var("NIX_PBT_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// The result of evaluating one expression with one evaluator.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Evaluated successfully to this JSON value.
    Value(Json),
    /// Evaluated successfully but printed something that is not JSON.
    Unparseable(String),
    /// An ordinary evaluation error; holds the message.
    Error(String),
    /// Killed by a signal, exited unexpectedly, timed out, or reported an
    /// internal error.
    Crash(String),
}

impl Outcome {
    /// Whether two outcomes count as equivalent: equal values, or both
    /// errors. A crash never agrees with anything.
    pub fn agrees_with(&self, other: &Outcome) -> bool {
        match (self, other) {
            (Outcome::Value(a), Outcome::Value(b)) => a == b,
            (Outcome::Unparseable(a), Outcome::Unparseable(b)) => a == b,
            (Outcome::Error(_), Outcome::Error(_)) => true,
            _ => false,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Outcome::Value(v) => v.to_string(),
            Outcome::Unparseable(s) => format!("<non-JSON output> {s:?}"),
            Outcome::Error(e) => format!("<evaluation error> {}", first_error_line(e)),
            Outcome::Crash(e) => format!("<CRASH> {}", e.trim()),
        }
    }

    fn from_json_bytes(bytes: &[u8]) -> Outcome {
        match serde_json::from_slice(bytes) {
            Ok(v) => Outcome::Value(v),
            Err(_) => Outcome::Unparseable(String::from_utf8_lossy(bytes).into_owned()),
        }
    }
}

/// The most informative line of an error message: the last `error:` line
/// with text after it (CppNix and Lix put the actual message last, after the
/// trace), or the first non-empty line.
fn first_error_line(msg: &str) -> String {
    let msg = strip_ansi(msg);
    let line = msg
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("error:") && l.len() > "error:".len())
        .or_else(|| msg.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    line.to_string()
}

/// Remove ANSI escape sequences (`ESC [ ... letter`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Evaluate `expr` with every evaluator in parallel.
pub fn eval_all(evs: &[Evaluator], expr: &str) -> Vec<Outcome> {
    std::thread::scope(|s| {
        let handles: Vec<_> = evs.iter().map(|ev| s.spawn(|| ev.eval(expr))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn looks_like_crash(stderr: &str) -> bool {
    const MARKERS: &[&str] = &[
        "This is a bug",
        "crashed",
        "Assertion `",
        "terminate called",
        "panicked at",
        "Segmentation fault",
    ];
    MARKERS.iter().any(|m| stderr.contains(m))
}

// ---------------------------------------------------------------------------
// exec mode

fn exec_once(ev: &Evaluator, expr: &str) -> Outcome {
    let mut child = match ev.command().arg(expr).stdin(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(e) => panic!(
            "could not run evaluator {} ({:?}): {e}",
            ev.name, ev.argv[0]
        ),
    };

    // Drain both pipes on their own threads so a chatty child can't block.
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out_pipe.read_to_end(&mut buf);
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err_pipe.read_to_end(&mut buf);
        buf
    });

    let status = match child.wait_timeout(timeout()).unwrap() {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Crash(format!("timed out after {:?}", timeout()));
        }
    };
    let stdout = out_t.join().unwrap();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap()).into_owned();

    if looks_like_crash(&stderr) {
        return Outcome::Crash(stderr);
    }
    match status.code() {
        Some(0) => Outcome::from_json_bytes(&stdout),
        Some(_) => Outcome::Error(stderr),
        None => Outcome::Crash(format!("killed by signal ({status})\n{stderr}")),
    }
}

// ---------------------------------------------------------------------------
// long-lived sessions

enum ReadErr {
    Eof,
    Timeout,
}

/// The read end of a child's pipe, drained by a thread into a channel so
/// reads can time out.
struct Pipe {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    eof: bool,
}

impl Pipe {
    fn new(mut r: impl Read + Send + 'static) -> Pipe {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 65536];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(chunk[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Pipe {
            rx,
            buf: Vec::new(),
            eof: false,
        }
    }

    fn fill(&mut self, deadline: Instant) -> Result<(), ReadErr> {
        if self.eof {
            return Err(ReadErr::Eof);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left) {
            Ok(chunk) => {
                self.buf.extend(chunk);
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) => Err(ReadErr::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                self.eof = true;
                Err(ReadErr::Eof)
            }
        }
    }

    fn read_line(&mut self, deadline: Instant) -> Result<Vec<u8>, ReadErr> {
        loop {
            if let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=i).collect();
                line.pop();
                return Ok(line);
            }
            self.fill(deadline)?;
        }
    }

    fn read_exact(&mut self, n: usize, deadline: Instant) -> Result<Vec<u8>, ReadErr> {
        while self.buf.len() < n {
            self.fill(deadline)?;
        }
        Ok(self.buf.drain(..n).collect())
    }

    /// Everything readable right now, or within `wait` if the pipe is about
    /// to close (used to collect a crashing process's last words).
    fn drain(&mut self, wait: Duration) -> String {
        let deadline = Instant::now() + wait;
        loop {
            match self.rx.try_recv() {
                Ok(chunk) => self.buf.extend(chunk),
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {
                    if wait.is_zero() || self.fill(deadline).is_err() {
                        break;
                    }
                }
            }
        }
        String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned()
    }
}

struct Session {
    evaluator: String,
    child: Child,
    stdin: ChildStdin,
    stdout: Pipe,
    stderr: Pipe,
    evals: usize,
    healthy: bool,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NONCE: AtomicU64 = AtomicU64::new(0);

impl Session {
    fn spawn(ev: &Evaluator) -> Session {
        let mut child = ev.command().spawn().unwrap_or_else(|e| {
            panic!(
                "could not run evaluator {} ({:?}): {e}",
                ev.name, ev.argv[0]
            )
        });
        let stdin = child.stdin.take().unwrap();
        let stdout = Pipe::new(child.stdout.take().unwrap());
        let stderr = Pipe::new(child.stderr.take().unwrap());
        Session {
            evaluator: ev.name.clone(),
            child,
            stdin,
            stdout,
            stderr,
            evals: 0,
            healthy: true,
        }
    }

    fn eval(&mut self, mode: Mode, expr: &str) -> Outcome {
        match mode {
            Mode::Repl => self.eval_repl(expr),
            Mode::Server => self.eval_server(expr),
            Mode::Exec => unreachable!(),
        }
    }

    /// Mark the session dead and describe why.
    fn fail(&mut self, err: ReadErr) -> Outcome {
        self.healthy = false;
        match err {
            ReadErr::Timeout => {
                let _ = self.child.kill();
                let stderr = self.stderr.drain(Duration::ZERO);
                Outcome::Crash(format!("timed out after {:?}\n{stderr}", timeout()))
            }
            ReadErr::Eof => {
                let stderr = self.stderr.drain(Duration::from_secs(2));
                let status = self
                    .child
                    .wait_timeout(Duration::from_secs(2))
                    .ok()
                    .flatten()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "still running".into());
                let msg = format!(
                    "{} session exited unexpectedly ({status})\n{stderr}",
                    self.evaluator
                );
                // Some REPLs exit on certain evaluation errors (Lix does on
                // stack overflows). That's the REPL's problem, not an
                // evaluation difference: count it as the error it printed.
                if !looks_like_crash(&stderr) && strip_ansi(&stderr).contains("error:") {
                    Outcome::Error(msg)
                } else {
                    Outcome::Crash(msg)
                }
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), ReadErr> {
        self.stdin
            .write_all(bytes)
            .and_then(|_| self.stdin.flush())
            .map_err(|_| ReadErr::Eof)
    }

    fn eval_server(&mut self, expr: &str) -> Outcome {
        self.evals += 1;
        let deadline = Instant::now() + timeout();
        let mut req = format!("{}\n", expr.len()).into_bytes();
        req.extend_from_slice(expr.as_bytes());
        if let Err(e) = self.send(&req) {
            return self.fail(e);
        }
        let header = match self.stdout.read_line(deadline) {
            Ok(h) => String::from_utf8_lossy(&h).into_owned(),
            Err(e) => return self.fail(e),
        };
        let (status, len) = match header.split_once(' ').map(|(s, n)| (s, n.parse::<usize>())) {
            Some((s, Ok(n))) if s == "ok" || s == "error" => (s.to_string(), n),
            _ => {
                self.healthy = false;
                return Outcome::Crash(format!("bad server response header {header:?}"));
            }
        };
        let body = match self.stdout.read_exact(len, deadline) {
            Ok(b) => b,
            Err(e) => return self.fail(e),
        };
        // Stderr is not part of the protocol; just keep it from piling up.
        let _ = self.stderr.drain(Duration::ZERO);
        if status == "ok" {
            Outcome::from_json_bytes(&body)
        } else {
            let msg = String::from_utf8_lossy(&body).into_owned();
            if looks_like_crash(&msg) {
                Outcome::Crash(msg)
            } else {
                Outcome::Error(msg)
            }
        }
    }

    fn eval_repl(&mut self, expr: &str) -> Outcome {
        assert!(
            !expr.contains('\n'),
            "repl mode needs single-line expressions: {expr:?}"
        );
        // Lix ≥ 2.96 applies backspaces in its input, even from a pipe.
        // Generated expressions only contain raw control characters inside
        // string literals, where an interpolation means the same thing.
        let expr = expr.replace('\u{8}', r#"${builtins.fromJSON "\"\\b\""}"#);
        self.evals += 1;
        let deadline = Instant::now() + timeout();
        let nonce = format!(
            "nix-pbt-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let input = format!("builtins.toJSON ({expr})\nbuiltins.trace \"{nonce}\" \"{nonce}\"\n");
        if let Err(e) = self.send(input.as_bytes()) {
            return self.fail(e);
        }

        let mut values = Vec::new();
        let value_end = format!("\"{nonce}\"");
        loop {
            match self.stdout.read_line(deadline) {
                Ok(line) => {
                    let line = strip_ansi(&String::from_utf8_lossy(&line));
                    let line = strip_prompts(&line);
                    // The sentinel is normally a line of its own, but don't
                    // rely on the previous output ending in a newline.
                    let (line, done) = match line.find(&value_end) {
                        Some(i) => (&line[..i], true),
                        None => (line, false),
                    };
                    if !line.trim().is_empty() {
                        values.push(line.trim().to_string());
                    }
                    if done {
                        break;
                    }
                }
                Err(e) => return self.fail(e),
            }
        }
        let mut stderr = String::new();
        let trace_end = format!("trace: {nonce}");
        loop {
            match self.stderr.read_line(deadline) {
                Ok(line) => {
                    let line = strip_ansi(&String::from_utf8_lossy(&line));
                    // Not necessarily at the start of a line: error messages
                    // quote source lines, and a raw `ESC ]` in a string
                    // literal makes Lix's ANSI filter swallow the rest of
                    // the message, newline included.
                    if let Some(i) = line.find(&trace_end) {
                        stderr.push_str(&line[..i]);
                        break;
                    }
                    stderr.push_str(&line);
                    stderr.push('\n');
                }
                Err(e) => return self.fail(e),
            }
        }

        if looks_like_crash(&stderr) {
            self.healthy = false;
            return Outcome::Crash(stderr);
        }
        match values.as_slice() {
            [] => Outcome::Error(stderr),
            [v] => match decode_string_literal(v) {
                Some(json) => Outcome::from_json_bytes(&json),
                None => Outcome::Unparseable(v.clone()),
            },
            _ => Outcome::Unparseable(values.join("\n")),
        }
    }
}

/// Remove REPL prompts from the start of an output line. Lix ≥ 2.96 prints
/// `nix-repl> ` to stdout when `TERM=dumb`, even if stdin isn't a terminal.
fn strip_prompts(mut line: &str) -> &str {
    const PROMPTS: &[&str] = &["nix-repl> ", "nix-repl>"];
    while let Some(rest) = PROMPTS.iter().find_map(|p| line.strip_prefix(p)) {
        line = rest;
    }
    line
}

/// Decode a string literal as printed by a REPL: either Nix syntax (`\"`,
/// `\\`, `\n`, `\r`, `\t`, `\$`) or JSON (additionally `\uXXXX`, `\/`, `\b`,
/// `\f`). The two can't be confused: Nix output escapes every backslash, so
/// it never contains `\u`, and JSON never contains `\$`.
pub fn decode_string_literal(s: &str) -> Option<Vec<u8>> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'u' => {
                let hex = |chars: &mut std::str::Chars| -> Option<u32> {
                    let h: String = chars.by_ref().take(4).collect();
                    u32::from_str_radix(&h, 16).ok()
                };
                let hi = hex(&mut chars)?;
                let cp = if (0xd800..0xdc00).contains(&hi) {
                    // Surrogate pair.
                    if chars.next()? != '\\' || chars.next()? != 'u' {
                        return None;
                    }
                    let lo = hex(&mut chars)?;
                    0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                } else {
                    hi
                };
                out.push(char::from_u32(cp)?);
            }
            other => out.push(other),
        }
    }
    Some(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_literals() {
        let d = |s: &str| String::from_utf8(decode_string_literal(s).unwrap()).unwrap();
        assert_eq!(d(r#""[1,\"a\\nb\${x}\"]""#), r#"[1,"a\nb${x}"]"#);
        assert_eq!(d(r#""é😀\n""#), "é😀\n");
    }

    #[test]
    fn ansi() {
        assert_eq!(strip_ansi("\x1b[35;1m\"3\"\x1b[0m"), "\"3\"");
    }

    #[test]
    fn prompts() {
        assert_eq!(strip_prompts("nix-repl> nix-repl> \"1\""), "\"1\"");
        assert_eq!(strip_prompts("nix-repl>"), "");
        assert_eq!(strip_prompts("\"nix-repl> \""), "\"nix-repl> \"");
    }

    #[test]
    fn modes() {
        assert_eq!(Evaluator::parse("a=repl:nix repl").mode, Mode::Repl);
        assert_eq!(Evaluator::parse("a= server:x --y").argv, ["x", "--y"]);
        assert_eq!(Evaluator::parse("a=nix-instantiate").mode, Mode::Exec);
    }
}
