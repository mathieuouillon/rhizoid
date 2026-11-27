//! The `rhizoid` command, run as a user runs it: every subcommand on the
//! fixtures, its output, its exit status, and its errors.

use std::path::PathBuf;
use std::process::Command;

fn data(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../rhizoid-io/tests/data")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Runs `rhizoid` with `args`: (success, stdout, stderr).
fn rhizoid(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rhizoid"))
        .args(args)
        .output()
        .expect("run rhizoid");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Runs a command that must succeed; its stdout.
fn ok(args: &[&str]) -> String {
    let (success, stdout, stderr) = rhizoid(args);
    assert!(success, "rhizoid {args:?} failed: {stderr}");
    stdout
}

/// Runs a command that must fail; its stderr.
fn fails(args: &[&str]) -> String {
    let (success, stdout, stderr) = rhizoid(args);
    assert!(!success, "rhizoid {args:?} succeeded: {stdout}");
    stderr
}

#[test]
fn ls_lists_keys_directories_and_details() {
    let dirs = data("root_dirs.root");
    let top = ok(&["ls", &dirs]);
    assert!(top.contains("n;1") && top.contains("TNamed"), "{top}");
    assert!(top.contains("a;1") && top.contains("TDirectory"), "{top}");
    assert!(!top.contains("in a/b"), "not recursive: {top}");
    let all = ok(&["ls", "-r", &dirs]);
    assert!(all.contains("in a/b"), "{all}");
    let sub = ok(&["ls", "-l", &format!("{dirs}:a/b")]);
    assert!(
        sub.contains("n;1") && sub.contains("0x"),
        "offsets in long form: {sub}"
    );
    assert!(!sub.contains("TDirectory"), "{sub}");
}

#[test]
fn check_and_header_describe_a_file() {
    let tree = data("root_tree.root");
    let check = ok(&["check", &tree]);
    assert!(
        check.contains("records") && check.trim_end().ends_with("OK"),
        "{check}"
    );
    let header = ok(&["header", &tree]);
    assert!(
        header.contains("version") && header.contains("64200"),
        "{header}"
    );
    assert!(header.contains("compression"), "{header}");
}

#[test]
fn dump_prints_members() {
    let n = ok(&["dump", &format!("{}:a/b/n", data("root_dirs.root"))]);
    assert!(n.starts_with("TNamed"), "{n}");
    assert!(n.contains("fTitle: \"in a/b\""), "{n}");
    let full = ok(&[
        "dump",
        "--full",
        &format!("{}:h1d", data("root_hists.root")),
    ]);
    assert!(full.contains("fEntries"), "{full}");
}

#[test]
fn tree_rntuple_and_scan_show_columns() {
    let tree = data("root_tree.root");
    let t = ok(&["tree", &format!("{tree}:t")]);
    assert!(t.contains("20 entries"), "{t}");
    for b in ["i32", "f64", "jag", "str"] {
        assert!(t.lines().any(|l| l.starts_with(b)), "branch {b}: {t}");
    }
    let nt = ok(&["rntuple", &format!("{}:t", data("root_ntuple.root"))]);
    assert!(
        nt.contains("format 1.1.0.2") && nt.contains("std::int32_t"),
        "{nt}"
    );
    let scan = ok(&["scan", &format!("{tree}:t"), "-b", "i32,jag,str", "-n", "2"]);
    assert!(
        scan.contains("-100000") && scan.contains("\"entry 1\""),
        "{scan}"
    );
    assert_eq!(scan.lines().count(), 3, "a header and two entries: {scan}");
    let window = ok(&[
        "scan",
        &format!("{}:t", data("root_ntuple.root")),
        "-b",
        "f64",
        "-s",
        "3",
        "-n",
        "2",
    ]);
    assert!(window.contains("0.75"), "entry 3 is 0.75: {window}");
}

#[test]
fn streamers_lists_class_descriptions() {
    let all = ok(&["streamers", &data("root_hists.root")]);
    assert!(all.contains("TH1D") && all.contains("TAxis"), "{all}");
    let one = ok(&["streamers", &data("root_hists.root"), "-c", "TNamed"]);
    assert!(one.contains("fName") && !one.contains("TH1D"), "{one}");
}

#[test]
fn new_writes_a_valid_empty_file() {
    let dir = std::env::temp_dir().join(format!("rhizoid-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp");
    let path = dir.join("empty.root").to_string_lossy().into_owned();
    ok(&["new", &path]);
    assert!(ok(&["check", &path]).trim_end().ends_with("OK"));
    assert_eq!(ok(&["ls", &path]).trim(), "");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn errors_name_what_went_wrong_once() {
    let missing = fails(&["ls", &data("nothing.root")]);
    assert!(missing.starts_with("rhizoid: cannot open"), "{missing}");
    // The cause is told once, not repeated by the error chain.
    let os = missing
        .trim_end()
        .rsplit(": ")
        .next()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(missing.matches(&os).count(), 1, "{missing}");

    let not_root = fails(&["check", &data("root_hists.txt")]);
    assert!(not_root.contains("not a ROOT file"), "{not_root}");

    let key = fails(&["dump", &format!("{}:nothing", data("root_dirs.root"))]);
    assert!(
        key.contains("no key `nothing` in the top directory"),
        "{key}"
    );
    let nested = fails(&["dump", &format!("{}:a/b/nothing", data("root_dirs.root"))]);
    assert!(nested.contains("in directory `a/b`"), "{nested}");

    let (success, _, usage) = rhizoid(&["frobnicate"]);
    assert!(!success && usage.contains("Usage"), "{usage}");
}
