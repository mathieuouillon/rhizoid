//! M2's exit criterion: every branch of every tree in a corpus of
//! ROOT-written files, compared with what uproot reads.
//!
//! Runs when `RHIZOID_CORPUS` names a directory of `.root` files and
//! `RHIZOID_ORACLE_PYTHON` a Python with uproot. Writes a coverage report
//! to `RHIZOID_CORPUS_REPORT` (default: `corpus-report.md` in the temp dir).
//! Fails on any branch we read that disagrees with uproot; branches we do
//! not support yet are counted, not failed.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

mod support;

use std::path::{Path, PathBuf};
use support::leaves::{column_leaves, tolerance};

use rhizoid_io::tree::Branch;
use rhizoid_io::{Directory, File, Tree};

const ENTRY_STOP: u64 = 200;

/// Branches where uproot is known to read wrongly, and why. For these, our
/// values must equal the start of uproot's values in every entry.
const UPROOT_OVER_READS: &[(&str, &str, &str)] = &[(
    "uproot-Event.root",
    "fTracks.fVertex[3]",
    "uproot reads 3x the values of a Double32_t[3] member of a split \
     TClonesArray, running into the next entries and then uninitialised \
     memory (1e-310); only entry 0's start can be compared, and each of our \
     entries holds 3 values per track (fTracks)",
)];

#[derive(Debug, Default)]
struct Tally {
    matched: usize,
    unverified: usize,
    groups: usize,
    unsupported: usize,
    differs: Vec<String>,
    unsupported_why: BTreeMap<String, usize>,
}

fn trees(dir: &Directory, prefix: &str, out: &mut Vec<String>) {
    let mut seen = Vec::new();
    for k in dir.keys() {
        let name = format!("{prefix}{}", k.name());
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        match k.class_name() {
            "TTree" | "TNtuple" | "TNtupleD" => out.push(name),
            "TDirectory" | "TDirectoryFile" => {
                if let Ok(sub) = dir.subdir(k.name()) {
                    trees(&sub, &format!("{name}/"), out);
                }
            }
            _ => {}
        }
    }
}

fn uproot(
    python: &str,
    file: &Path,
    trees: &[String],
) -> HashMap<(String, String), (String, Vec<String>)> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross/uproot_leaves.py");
    let out = std::process::Command::new(python)
        .arg(script)
        .arg(file)
        .arg(ENTRY_STOP.to_string())
        .args(trees)
        .output()
        .expect("run uproot_leaves.py");
    assert!(
        out.status.success(),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut map: HashMap<(String, String), (String, Vec<String>)> = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut it = line.split('\t');
        let (Some(t), Some(b), Some(kind)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let rest: Vec<String> = it.map(str::to_owned).collect();
        let key = (t.to_owned(), b.to_owned());
        match kind {
            "counts" => {
                let counts = rest
                    .first()
                    .map(|c| {
                        c.split(' ')
                            .filter(|x| !x.is_empty())
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                map.insert(key.clone(), ("counts".to_owned(), counts));
            }
            "leaves" => {
                let counts = map.remove(&key).map(|(_, c)| c).unwrap_or_default();
                map.insert(
                    key,
                    (
                        counts.join(" "),
                        rest.into_iter().filter(|x| !x.is_empty()).collect(),
                    ),
                );
            }
            _ => {
                map.insert(key, ("error".to_owned(), rest));
            }
        }
    }
    map
}

/// What comparing one branch with uproot found.
enum Outcome {
    Match,
    Unverified,
    Group,
    Unsupported(String),
    Differs(String),
}

fn walk<'a>(b: &'a Branch, out: &mut Vec<&'a Branch>) {
    out.push(b);
    for sub in b.branches() {
        walk(sub, out);
    }
}

/// Compares one branch with uproot's leaves (`counts`, `values`).
fn compare_branch(
    tree: &Tree,
    b: &Branch,
    counts: &str,
    values: &[String],
    over_read: bool,
) -> Outcome {
    if b.is_group() {
        return Outcome::Group;
    }
    let interp = match b.interpretation() {
        Ok(i) => i.clone(),
        Err(why) => {
            let why = why.to_string();
            return Outcome::Unsupported(why.split(" (").next().unwrap_or_default().to_owned());
        }
    };
    let col = match tree.read(b.name(), 0..ENTRY_STOP.min(b.num_entries())) {
        Ok(c) => c,
        Err(e) => return Outcome::Differs(format!("read failed: {e}")),
    };
    if counts == "error" {
        // We read it; uproot cannot check it.
        return Outcome::Unverified;
    }
    let Some(ours) = column_leaves(&col) else {
        return Outcome::Unsupported("kept undecoded".to_owned());
    };
    let tol = tolerance(&interp);
    let their_counts: Vec<usize> = counts.split(' ').filter_map(|c| c.parse().ok()).collect();
    if their_counts.len() != ours.len() {
        return Outcome::Differs(format!(
            "{} entries vs uproot's {}",
            ours.len(),
            their_counts.len()
        ));
    }
    let mut at = 0;
    for (i, (entry, &n)) in ours.iter().zip(&their_counts).enumerate() {
        if over_read && i > 0 {
            break;
        }
        let theirs = values.get(at..at + n).unwrap_or_default();
        at += n;
        if !(entry.len() == n || (over_read && entry.len() <= n)) {
            return Outcome::Differs(format!("entry {i}: {} leaves vs uproot's {n}", entry.len()));
        }
        if let Some(j) = entry
            .iter()
            .zip(theirs)
            .position(|(o, th)| !o.matches(th, tol))
        {
            return Outcome::Differs(format!(
                "entry {i}, leaf {j}: ours {:?}, uproot {}",
                entry[j], theirs[j]
            ));
        }
    }
    Outcome::Match
}

/// Compares every branch of every tree of one file; returns the report row.
fn compare_file(python: &str, path: &Path, total: &mut Tally) -> Option<String> {
    let file = File::open(path).expect("open");
    let mut names = Vec::new();
    trees(&file.root(), "", &mut names);
    if names.is_empty() {
        return None;
    }
    let fname = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let theirs = uproot(python, path, &names);
    let mut t = Tally::default();
    let mut branches = 0;
    for tname in &names {
        let tree = match file.tree(tname) {
            Ok(tree) => tree,
            Err(e) => {
                t.differs
                    .push(format!("{tname}: cannot open the tree: {e}"));
                continue;
            }
        };
        let mut all = Vec::new();
        for b in tree.branches() {
            walk(b, &mut all);
        }
        for b in all {
            branches += 1;
            let Some((counts, values)) = theirs.get(&(tname.clone(), b.name().to_owned())) else {
                // uproot does not list it: we must still read it.
                let read = b.interpretation().is_ok()
                    && !b.is_group()
                    && tree
                        .read(b.name(), 0..ENTRY_STOP.min(b.num_entries()))
                        .is_err();
                if read {
                    t.differs.push(format!(
                        "{tname}:{}: read failed (uproot does not list it)",
                        b.name()
                    ));
                }
                continue;
            };
            let over_read = UPROOT_OVER_READS
                .iter()
                .any(|(f, br, _)| *f == fname && *br == b.name());
            match compare_branch(&tree, b, counts, values, over_read) {
                Outcome::Match => t.matched += 1,
                Outcome::Unverified => t.unverified += 1,
                Outcome::Group => t.groups += 1,
                Outcome::Unsupported(why) => {
                    t.unsupported += 1;
                    *t.unsupported_why.entry(why).or_default() += 1;
                }
                Outcome::Differs(d) => t.differs.push(format!("{tname}:{}: {d}", b.name())),
            }
        }
    }
    let row = format!(
        "| {fname} | {} | {branches} | {} | {} | {} | {} |",
        names.len(),
        t.groups,
        t.matched,
        t.unsupported,
        t.differs.len()
    );
    total.matched += t.matched;
    total.unverified += t.unverified;
    total.groups += t.groups;
    total.unsupported += t.unsupported;
    total
        .differs
        .extend(t.differs.into_iter().map(|d| format!("{fname}: {d}")));
    for (k, v) in t.unsupported_why {
        *total.unsupported_why.entry(k).or_default() += v;
    }
    Some(row)
}

fn render(rows: &[String], total: &Tally) -> String {
    let mut report = String::from(
        "# Corpus coverage\n\n| file | trees | branches | groups | match | unsupported | differ |\n|---|---|---|---|---|---|---|\n",
    );
    for row in rows {
        let _ = writeln!(report, "{row}");
    }
    let _ = writeln!(
        report,
        "\n**{} branches match uproot, {} not supported yet, {} differ** ({} groups of sub-branches hold no values of their own; {} more are read without error where uproot cannot read them).\n",
        total.matched,
        total.unsupported,
        total.differs.len(),
        total.groups,
        total.unverified
    );
    let _ = writeln!(report, "## Not supported yet\n");
    for (why, n) in &total.unsupported_why {
        let _ = writeln!(report, "- {n} × {why}");
    }
    let _ = writeln!(
        report,
        "\n## Known uproot errors (our values checked against uproot's valid part)\n"
    );
    for (f, b, why) in UPROOT_OVER_READS {
        let _ = writeln!(report, "- {f} `{b}`: {why}");
    }
    let _ = writeln!(
        report,
        "\nPacked floats (`Float16_t`, `Double32_t`) and floats inside objects are compared to 1e-6 relative: we use ROOT's formulas (`aint / factor + min`), uproot an approximation."
    );
    if !total.differs.is_empty() {
        let _ = writeln!(report, "\n## Differences\n");
        for d in &total.differs {
            let _ = writeln!(report, "- {d}");
        }
    }
    report
}

#[test]
fn corpus_matches_uproot() {
    let (Ok(dir), Ok(python)) = (
        std::env::var("RHIZOID_CORPUS"),
        std::env::var("RHIZOID_ORACLE_PYTHON"),
    ) else {
        return;
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "root"))
        .collect();
    files.sort();
    let mut total = Tally::default();
    let rows: Vec<String> = files
        .iter()
        .filter_map(|p| compare_file(&python, p, &mut total))
        .collect();
    let report = render(&rows, &total);
    let out = std::env::var("RHIZOID_CORPUS_REPORT").map_or_else(
        |_| std::env::temp_dir().join("corpus-report.md"),
        PathBuf::from,
    );
    std::fs::write(&out, &report).expect("write report");
    assert!(
        total.differs.is_empty(),
        "{} branches differ from uproot; see {}",
        total.differs.len(),
        out.display()
    );
}
