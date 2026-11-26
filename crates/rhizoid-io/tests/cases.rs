//! The data-driven suite (ARCHITECTURE §9.3, after regex's): every case of
//! `tests/cases/*.toml`, against every source: in memory, a local file, a
//! local HTTP server answering multi-range requests and one answering
//! single ranges only (feature `http`), and the async API (feature
//! `async`). Each must give the expected outcome, and all the same one, so
//! a bug in one source or path shows as a difference. New cases need no
//! Rust code.
//!
//! A case names a file (in `tests/data`, else in `RHIZOID_CORPUS`, else it
//! is skipped) and one check:
//!
//! ```toml
//! [[case]]
//! name   = "root-tree-f64"
//! file   = "root_tree.root"
//! tree   = "t"             # or `rntuple = "t"`
//! branch = "f64"           # (`field` for an RNTuple)
//! range  = [0, 20]         # optional
//! count  = 20              # values, arrays and collections flattened
//! sum    = 47.5            # their sum
//!
//! [[case]]
//! name   = "a-histogram"
//! file   = "root_hists.root"
//! object = "h1d"
//! class  = "TH1D"          # or `error = "NotFound"` for any check
//! ```
//!
//! Expected values come from ROOT itself (`tests/cross/root_sums.C`), or
//! from how the fixture was written.

#![cfg(not(target_arch = "wasm32"))]

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rhizoid_io::columnar::Column;
use rhizoid_io::{File, ReadOptions, Value};

/// A value of the TOML subset the cases use.
#[derive(Clone, Debug, PartialEq)]
enum Toml {
    Str(String),
    Num(f64),
    List(Vec<Toml>),
}

/// Parses `[[case]]` tables of `key = value` lines (strings, numbers,
/// lists of numbers); `#` starts a comment.
fn parse(text: &str) -> Vec<BTreeMap<String, Toml>> {
    fn value(v: &str) -> Toml {
        let v = v.trim();
        if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Toml::Str(s.to_owned())
        } else if let Some(l) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            Toml::List(
                l.split(',')
                    .filter(|x| !x.trim().is_empty())
                    .map(value)
                    .collect(),
            )
        } else {
            Toml::Num(
                v.replace('_', "")
                    .parse()
                    .map_err(|e| format!("bad value {v}: {e}"))
                    .expect("a string, a number or a list"),
            )
        }
    }
    let mut cases = Vec::new();
    for line in text.lines() {
        // A `#` outside a string starts a comment.
        let mut quoted = false;
        let end = line
            .char_indices()
            .find(|&(_, c)| {
                if c == '"' {
                    quoted = !quoted;
                }
                c == '#' && !quoted
            })
            .map_or(line.len(), |(i, _)| i);
        let line = &line[..end];
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "[[case]]" {
            cases.push(BTreeMap::new());
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("bad line {line}"))
            .expect("key = value");
        cases
            .last_mut()
            .expect("a key before [[case]]")
            .insert(k.trim().to_owned(), value(v));
    }
    cases
}

fn text<'a>(case: &'a BTreeMap<String, Toml>, key: &str) -> Option<&'a str> {
    match case.get(key) {
        Some(Toml::Str(s)) => Some(s),
        _ => None,
    }
}

fn number(case: &BTreeMap<String, Toml>, key: &str) -> Option<f64> {
    match case.get(key) {
        Some(Toml::Num(n)) => Some(*n),
        _ => None,
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "case data"
)]
fn range(case: &BTreeMap<String, Toml>) -> Option<std::ops::Range<u64>> {
    match case.get("range") {
        Some(Toml::List(l)) => {
            let ends = match l.as_slice() {
                [Toml::Num(a), Toml::Num(b)] => Some((*a, *b)),
                _ => None,
            };
            let (a, b) = ends.expect("range = [first, end]");
            Some(a as u64..b as u64)
        }
        _ => None,
    }
}

fn push_values(v: &Value, out: &mut Vec<f64>) {
    match v {
        Value::Array(a) => out.extend(a.to_f64_vec()),
        Value::List(items) => items.iter().for_each(|i| push_values(i, out)),
        v => {
            if let Some(x) = v.as_f64() {
                out.push(x);
            }
        }
    }
}

/// A column's values, flattened (a string as its length).
#[expect(clippy::cast_precision_loss, reason = "string lengths")]
fn values(col: &Column) -> Vec<f64> {
    let mut out = Vec::new();
    match col {
        Column::Values(a) | Column::Fixed { values: a, .. } | Column::Jagged { values: a, .. } => {
            out.extend(a.to_f64_vec());
        }
        Column::Objects(v) => v.iter().for_each(|x| push_values(x, &mut out)),
        Column::Strings(s) => out.extend(s.iter().map(|s| s.len() as f64)),
        _ => {}
    }
    out
}

/// The outcome of a case, as text: `count … sum …`, `class …`, `error …`.
fn summary(values: &[f64]) -> String {
    let sum: f64 = values.iter().sum();
    format!("count {} sum {sum:?}", values.len())
}

fn error(e: &rhizoid_io::Error) -> String {
    format!("error {:?}", e.kind())
}

/// What a case names when it checks neither an object nor a tree.
const CHECKS: &str = "a case checks an object, a tree or an RNTuple";

/// A case's outcome through a synchronous `File`.
fn outcome(file: &rhizoid_io::Result<File>, case: &BTreeMap<String, Toml>) -> String {
    let file = match file {
        Ok(f) => f,
        Err(e) => return error(e),
    };
    let result = (|| -> rhizoid_io::Result<String> {
        if let Some(object) = text(case, "object") {
            return Ok(format!("class {}", file.get_dyn(object)?.class()));
        }
        let (col, total) = if let Some(tree) = text(case, "tree") {
            let t = file.tree(tree)?;
            let branch = text(case, "branch").expect("branch");
            let n = t.num_entries();
            (t.read(branch, range(case).unwrap_or(0..n))?, n)
        } else {
            let nt = file.rntuple(text(case, "rntuple").expect(CHECKS))?;
            let field = text(case, "field").expect("field");
            let n = nt.num_entries();
            (nt.read(field, range(case).unwrap_or(0..n))?, n)
        };
        let _ = total;
        Ok(summary(&values(&col)))
    })();
    result.unwrap_or_else(|e| error(&e))
}

#[cfg(feature = "async")]
mod asynchronous {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);

    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
            std::thread::park();
        }
    }

    /// A case's outcome through the async API.
    pub(super) fn outcome(bytes: Vec<u8>, case: &BTreeMap<String, Toml>) -> String {
        block_on(async {
            let file = match File::open_async(bytes).await {
                Ok(f) => f,
                Err(e) => return error(&e),
            };
            let result = async {
                if let Some(object) = text(case, "object") {
                    return Ok(format!("class {}", file.get_dyn(object).await?.class()));
                }
                let col = if let Some(tree) = text(case, "tree") {
                    let t = file.tree(tree).await?;
                    let branch = text(case, "branch").expect("branch");
                    let n = t.num_entries();
                    t.read(branch, range(case).unwrap_or(0..n)).await?
                } else {
                    let nt = file.rntuple(text(case, "rntuple").expect(CHECKS)).await?;
                    let field = text(case, "field").expect("field");
                    let n = nt.num_entries();
                    nt.read(field, range(case).unwrap_or(0..n)).await?
                };
                Ok::<_, rhizoid_io::Error>(summary(&values(&col)))
            };
            result.await.unwrap_or_else(|e| error(&e))
        })
    }
}

/// The expected outcome of a case.
fn expected(case: &BTreeMap<String, Toml>) -> Expect {
    if let Some(kind) = text(case, "error") {
        return Expect::Text(format!("error {kind}"));
    }
    if let Some(class) = text(case, "class") {
        return Expect::Text(format!("class {class}"));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "case data"
    )]
    let count = number(case, "count").expect("count") as usize;
    Expect::Sum {
        count,
        sum: number(case, "sum").expect("sum"),
    }
}

enum Expect {
    Text(String),
    Sum { count: usize, sum: f64 },
}

impl Expect {
    fn holds(&self, got: &str) -> bool {
        match self {
            Self::Text(t) => got == t,
            Self::Sum { count, sum } => {
                let Some(rest) = got.strip_prefix(&format!("count {count} sum ")) else {
                    return false;
                };
                let mine: f64 = rest.parse().unwrap_or(f64::NAN);
                (mine.is_nan() && sum.is_nan())
                    || (mine - sum).abs() <= 1e-9 * mine.abs().max(sum.abs())
            }
        }
    }
}

fn find(name: &str) -> Option<PathBuf> {
    let data = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    if data.exists() {
        return Some(data);
    }
    let corpus = PathBuf::from(std::env::var("RHIZOID_CORPUS").ok()?).join(name);
    corpus.exists().then_some(corpus)
}

#[test]
fn every_case_through_every_source() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("tests/cases")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    paths.sort();
    let mut run = 0;
    let mut failures = Vec::new();
    #[cfg(feature = "http")]
    let mut served: Vec<(String, Vec<u8>)> = Vec::new();
    let mut work = Vec::new();
    for path in &paths {
        let cases = parse(&std::fs::read_to_string(path).expect("read"));
        assert!(!cases.is_empty(), "{} has no case", path.display());
        for case in cases {
            let name = text(&case, "name").expect("name").to_owned();
            let file = text(&case, "file").expect("file");
            let Some(found) = find(file) else { continue };
            #[cfg(feature = "http")]
            if !served.iter().any(|(n, _)| n == file) {
                served.push((file.to_owned(), std::fs::read(&found).expect("read")));
            }
            work.push((name, file.to_owned(), found, case));
        }
    }
    #[cfg(feature = "http")]
    let servers = {
        let multi = support::server::Server::start(served.clone());
        let single = support::server::Server::start(served);
        single
            .behaviour
            .multipart
            .store(false, std::sync::atomic::Ordering::SeqCst);
        (multi, single)
    };
    for (name, file, path, case) in &work {
        let bytes = std::fs::read(path).expect("read");
        let mut outcomes: Vec<(&str, String)> = vec![
            (
                "memory",
                outcome(
                    &File::from_source(bytes.clone(), ReadOptions::default()),
                    case,
                ),
            ),
            ("file", outcome(&File::open(path), case)),
        ];
        #[cfg(feature = "http")]
        {
            let open = |url: String| File::open_url(&url);
            outcomes.push(("http", outcome(&open(servers.0.url(file)), case)));
            outcomes.push((
                "http, single ranges",
                outcome(&open(servers.1.url(file)), case),
            ));
        }
        #[cfg(feature = "async")]
        outcomes.push(("async", asynchronous::outcome(bytes, case)));
        let _ = file;
        let expect = expected(case);
        for (source, got) in &outcomes {
            if !expect.holds(got) {
                failures.push(format!("{name} through {source}: {got}"));
            }
        }
        run += 1;
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(run > 0, "no case ran");
}

#[test]
fn the_parser_reads_the_case_format() {
    let cases = parse(
        "# a comment\n[[case]]\nname = \"a # b\"  # trailing\nrange = [0, 10]\nsum = -1.5e3\n",
    );
    assert_eq!(cases.len(), 1);
    assert_eq!(text(&cases[0], "name"), Some("a # b"));
    assert_eq!(range(&cases[0]), Some(0..10));
    assert_eq!(number(&cases[0], "sum"), Some(-1500.0));
}
