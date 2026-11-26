//! Goal 1's measure (ARCHITECTURE §2, CEO-D4): the share of branches we
//! read as **ROOT itself** reads them, over the corpus.
//!
//! Runs when `RHIZOID_ROOT` names a `root` executable and `RHIZOID_CORPUS`
//! a directory of `.root` files (the fixtures in `tests/data` are added).
//! For every tree, `tests/cross/root_sums.C` has ROOT read every branch of
//! numbers the two ways ROOT reads data, entry by entry (`GetEntry` and the
//! leaf's values) and through `TTree::Draw`, and print the number of
//! values and their sum for each; we read the same branch and compare. A
//! match with either is a match with ROOT: each way has its blind spots
//! (Draw misreads baskets without entry offsets and reads `char` arrays as
//! strings; the leaf API misreads STL members of classes without a
//! dictionary). Every branch lands in one category: **match**, **differ**,
//! **unsupported** (ROOT reads it, we do not), or not checked because ROOT
//! does not draw it (strings, objects, several leaves) or crashes on it
//! (classes without a dictionary). The report, with the list of everything
//! not covered, goes to `RHIZOID_COVERAGE_REPORT` (default:
//! `coverage-report.md` in the temp dir). The test fails on any branch
//! that differs or that we cannot read.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rhizoid_io::columnar::Column;
use rhizoid_io::{File, Value};

/// ROOT's readings of a file's branches, by tree and branch.
type Readings = BTreeMap<(String, String), Root>;

/// What ROOT said about one branch: its readings (entry by entry, and
/// through `TTree::Draw`), or why it has none.
#[derive(Clone, Debug, Default)]
struct Root {
    entry: Option<(u64, f64)>,
    draw: Option<(u64, f64)>,
    skip: Option<String>,
    crash: bool,
}

/// ROOT's reading of every tree of a file: `(tree, branch) → result`.
fn root_sums(root: &str, file: &Path) -> Readings {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/root_sums.C");
    let mut out = BTreeMap::new();
    let mut from = 0usize;
    for _ in 0..200 {
        let run = std::process::Command::new(root)
            .args(["-b", "-q", "-l"])
            .arg(format!(
                "{}(\"{}\", {from})",
                script.display(),
                file.display()
            ))
            .output()
            .expect("run root");
        let stdout = String::from_utf8_lossy(&run.stdout);
        let mut pending: Option<(usize, String, String)> = None;
        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(5, ' ').collect();
            match parts.as_slice() {
                ["BEGIN", i, tree, branch] => {
                    pending = Some((
                        i.parse().unwrap_or(0),
                        (*tree).to_owned(),
                        (*branch).to_owned(),
                    ));
                }
                [kind @ ("SUM" | "DRAW"), tree, branch, count, sum] => {
                    let r: &mut Root = out
                        .entry(((*tree).to_owned(), (*branch).to_owned()))
                        .or_default();
                    let reading = (count.parse().unwrap_or(0), sum.parse().unwrap_or(f64::NAN));
                    if *kind == "SUM" {
                        pending = None;
                        r.entry = Some(reading);
                    } else {
                        r.draw = Some(reading);
                    }
                }
                ["NOREAD", tree, branch] => {
                    pending = None;
                    out.entry(((*tree).to_owned(), (*branch).to_owned()))
                        .or_default();
                }
                ["SKIP", tree, branch, why @ ..] => {
                    pending = None;
                    out.entry(((*tree).to_owned(), (*branch).to_owned()))
                        .or_default()
                        .skip = Some(why.join(" "));
                }
                _ => {}
            }
        }
        if stdout.lines().any(|l| l == "DONE") {
            return out;
        }
        // ROOT crashed on the branch it began last: note it, go on after it.
        let Some((i, tree, branch)) = pending else {
            out.entry((String::new(), String::new())).or_default().skip =
                Some("ROOT could not open the file".into());
            return out;
        };
        out.entry((tree, branch)).or_default().crash = true;
        from = i + 1;
    }
    out
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

/// Our reading of a branch: its values, flattened.
fn ours(file: &File, tree: &str, branch: &str) -> Result<Vec<f64>, String> {
    let t = file.tree(tree).map_err(|e| e.to_string())?;
    let col = t.read(branch, ..).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    match &col {
        Column::Values(a) | Column::Fixed { values: a, .. } | Column::Jagged { values: a, .. } => {
            out.extend(a.to_f64_vec());
        }
        Column::Objects(v) => v.iter().for_each(|x| push_values(x, &mut out)),
        Column::Strings(_) => return Err("strings".into()),
        _ => return Err("an unknown column".into()),
    }
    Ok(out)
}

fn same(sum: f64, theirs: f64) -> bool {
    (sum.is_nan() && theirs.is_nan())
        || (sum.is_infinite() && sum.to_bits() == theirs.to_bits())
        || (sum - theirs).abs() <= 1e-9 * sum.abs().max(theirs.abs())
}

/// `fBits` as ROOT reports it: the object's bits in memory, where ROOT
/// sets `kNotDeleted` (and `kIsOnHeap`) whatever the file holds.
fn same_bits(values: &[f64], theirs: f64) -> bool {
    [0u64, 0x0200_0000, 0x0300_0000].iter().any(|&add| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "bit words"
        )]
        let sum: f64 = values.iter().map(|&v| ((v as u64) | add) as f64).sum();
        same(sum, theirs)
    })
}

#[derive(Default)]
struct Tally {
    matched: usize,
    differ: Vec<String>,
    unsupported: Vec<String>,
    skipped: BTreeMap<String, usize>,
    crashed: Vec<String>,
}

fn files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(dir) = std::env::var("RHIZOID_CORPUS") {
        for e in std::fs::read_dir(dir)
            .expect("corpus")
            .filter_map(Result::ok)
        {
            out.push(e.path());
        }
    }
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    for e in std::fs::read_dir(data)
        .expect("data")
        .filter_map(Result::ok)
    {
        out.push(e.path());
    }
    out.retain(|p| p.extension().is_some_and(|x| x == "root"));
    out.sort();
    out
}

/// Compares our reading of one branch with ROOT's, into `tally`.
fn compare(
    tally: &mut Tally,
    file: &rhizoid_io::Result<File>,
    name: &str,
    tree: &str,
    branch: &str,
    r: &Root,
) {
    let id = format!("{name}: {tree}/{branch}");
    let readings: Vec<(u64, f64)> = r.entry.iter().chain(&r.draw).copied().collect();
    if readings.is_empty() {
        if r.crash {
            tally.crashed.push(id);
        } else {
            let why = r.skip.clone().unwrap_or_else(|| {
                "no reading: ROOT's leaf API fails without the class's dictionary".to_owned()
            });
            let kind = match why.split_once('=') {
                Some(("type", t)) => format!("type {t}"),
                _ => why,
            };
            *tally.skipped.entry(kind).or_default() += 1;
        }
        return;
    }
    let read = file
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|f| ours(f, tree, branch));
    match read {
        Err(e) => tally.unsupported.push(format!("{id} ({e})")),
        Ok(values) => {
            let mine: f64 = values.iter().sum();
            let agrees = |&(count, sum): &(u64, f64)| {
                values.len() as u64 == count
                    && (same(mine, sum) || (branch.ends_with("fBits") && same_bits(&values, sum)))
            };
            if readings.iter().any(agrees) {
                tally.matched += 1;
            } else {
                let theirs: Vec<String> = readings
                    .iter()
                    .map(|(c, s)| format!("{c} values, sum {s}"))
                    .collect();
                tally.differ.push(format!(
                    "{id}: ROOT {}; ours {} values, sum {mine}",
                    theirs.join(" / "),
                    values.len()
                ));
            }
        }
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "one report, written in order")]
fn coverage_against_root() {
    let (Ok(root), Ok(_)) = (
        std::env::var("RHIZOID_ROOT"),
        std::env::var("RHIZOID_CORPUS"),
    ) else {
        return;
    };
    let files = files();
    // Four ROOT processes at a time.
    let results: Vec<(PathBuf, Readings)> = std::thread::scope(|s| {
        let chunks: Vec<Vec<PathBuf>> = (0..4)
            .map(|k| files.iter().skip(k).step_by(4).cloned().collect())
            .collect();
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                let root = root.clone();
                s.spawn(move || {
                    chunk
                        .into_iter()
                        .map(|f| {
                            let sums = root_sums(&root, &f);
                            (f, sums)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("thread"))
            .collect()
    });

    let mut total = Tally::default();
    let mut table = String::from(
        "| file | branches ROOT reads | match | differ | unsupported | not read by ROOT | ROOT crashes |\n\
         |---|---|---|---|---|---|---|\n",
    );
    let mut results = results;
    results.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, sums) in &results {
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut tally = Tally::default();
        let file = File::open(path);
        for ((tree, branch), r) in sums {
            compare(&mut tally, &file, &name, tree, branch, r);
        }
        let skipped: usize = tally.skipped.values().sum();
        let drawn = tally.matched + tally.differ.len() + tally.unsupported.len();
        let _ = writeln!(
            table,
            "| {name} | {drawn} | {} | {} | {} | {skipped} | {} |",
            tally.matched,
            tally.differ.len(),
            tally.unsupported.len(),
            tally.crashed.len()
        );
        total.matched += tally.matched;
        total.differ.extend(tally.differ);
        total.unsupported.extend(tally.unsupported);
        total.crashed.extend(tally.crashed);
        for (k, n) in tally.skipped {
            *total.skipped.entry(k).or_default() += n;
        }
    }
    let drawn = total.matched + total.differ.len() + total.unsupported.len();
    let mut report = format!(
        "# Coverage against ROOT\n\n\
         Every branch of numbers in {} files, as ROOT 6.42 reads it (entry by entry, \
         and through `TTree::Draw`: the number of values and their sum, arrays and \
         collections flattened; a match with either is a match) and as rhizoid reads \
         it (`tests/coverage.rs`, `tests/cross/root_sums.C`).\n\n\
         **{} of {drawn} branches ROOT reads match ({:.1} %)**; {} differ; {} are not read by \
         rhizoid.\n\n",
        results.len(),
        total.matched,
        if drawn == 0 {
            100.0
        } else {
            #[expect(clippy::cast_precision_loss, reason = "a percentage")]
            let share = 100.0 * total.matched as f64 / drawn as f64;
            share
        },
        total.differ.len(),
        total.unsupported.len(),
    );
    report.push_str(&table);
    report.push_str("\n## Not covered\n\nBranches ROOT does not read as numbers (by reason):\n\n");
    for (k, n) in &total.skipped {
        let _ = writeln!(report, "- {k}: {n}");
    }
    let _ = writeln!(
        report,
        "\nBranches ROOT crashes on (classes without a dictionary), {}:\n",
        total.crashed.len()
    );
    for c in &total.crashed {
        let _ = writeln!(report, "- {c}");
    }
    for (title, list) in [
        ("Differ", &total.differ),
        ("Not read by rhizoid", &total.unsupported),
    ] {
        let _ = writeln!(report, "\n{title}, {}:\n", list.len());
        for l in list {
            let _ = writeln!(report, "- {l}");
        }
    }
    let out = std::env::var("RHIZOID_COVERAGE_REPORT").map_or_else(
        |_| std::env::temp_dir().join("coverage-report.md"),
        PathBuf::from,
    );
    std::fs::write(&out, &report).expect("write the report");
    assert!(
        total.differ.is_empty() && total.unsupported.is_empty(),
        "see {}:\n{}",
        out.display(),
        total
            .differ
            .iter()
            .chain(&total.unsupported)
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
