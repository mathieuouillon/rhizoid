//! End-to-end tests: write files, read them back, recover damaged ones, and
//! (optionally) cross-check with uproot.

use std::path::{Path, PathBuf};

use rhizoid_io::{
    DiagnosticKind, ErrorKind, File, ReadOptions, Strictness, WritableFile, WriteOptions,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rhizoid-it-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("temp dir");
        Self(p)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn empty_file_round_trips() {
    let dir = TempDir::new("empty");
    let path = dir.path("empty.root");
    rhizoid_io::create_with(&path, WriteOptions::default().title("a title"), |f| {
        assert_eq!(f.name(), "empty.root");
        Ok::<(), rhizoid_io::Error>(())
    })
    .expect("write");

    let file = File::open(&path).expect("open");
    assert_eq!(file.name(), "empty.root");
    assert_eq!(file.title(), "a title");
    assert_eq!(file.keys().count(), 0);
    assert_eq!(file.diagnostics(), []);
    assert_eq!(file.header().begin().get(), 100);
    assert_eq!(file.header().end().get(), file.len());
    let report = file.check().expect("check");
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(
        report.records, 4,
        "TFile key, StreamerInfo, key list, free list"
    );
    assert_eq!(report.free_segments, 1);
}

#[test]
fn closure_error_is_returned_and_file_is_still_closed() {
    #[derive(Debug)]
    struct MyError;
    impl From<rhizoid_io::Error> for MyError {
        fn from(_: rhizoid_io::Error) -> Self {
            Self
        }
    }
    let dir = TempDir::new("closure");
    let path = dir.path("f.root");
    let r: Result<(), MyError> = rhizoid_io::create(&path, |_| Err(MyError));
    assert!(r.is_err());
    let file = File::open(&path).expect("closed anyway (D7)");
    assert_eq!(file.diagnostics(), []);
}

fn unclosed(path: &Path) {
    let w = WritableFile::create(path).expect("create");
    drop(w); // no I/O in Drop: the header still says "not closed"
}

#[test]
fn unclosed_file_is_recovered_by_default() {
    let dir = TempDir::new("unclosed");
    let path = dir.path("u.root");
    unclosed(&path);
    let file = File::open(&path).expect("recovered (D16)");
    let diags = file.diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].kind(), DiagnosticKind::Recovered);
    assert_eq!(file.keys().count(), 0);
}

#[test]
fn strict_mode_refuses_unclosed_files() {
    let dir = TempDir::new("strict");
    let path = dir.path("u.root");
    unclosed(&path);
    let e = File::open_with(&path, ReadOptions::default().strictness(Strictness::Strict))
        .expect_err("strict");
    assert_eq!(e.kind(), ErrorKind::Strict);
}

#[test]
fn truncated_file_is_recovered() {
    let dir = TempDir::new("trunc");
    let path = dir.path("t.root");
    rhizoid_io::create(&path, |_| Ok::<(), rhizoid_io::Error>(())).expect("write");
    let bytes = std::fs::read(&path).expect("read");
    std::fs::write(&path, &bytes[..bytes.len() - 20]).expect("truncate");
    let file = File::open(&path).expect("recovered");
    assert_eq!(file.diagnostics()[0].kind(), DiagnosticKind::Recovered);
}

#[test]
fn garbage_is_not_a_root_file() {
    let dir = TempDir::new("garbage");
    let path = dir.path("g.root");
    std::fs::write(&path, b"PK\x03\x04 definitely a zip file, not ROOT").expect("write");
    assert_eq!(
        File::open(&path).expect_err("not root").kind(),
        ErrorKind::NotRootFile
    );
}

/// Cross-check with uproot when `RHIZOID_ORACLE_PYTHON` points at a Python
/// with uproot, lz4, zstandard and xxhash installed. Skipped otherwise.
#[test]
fn uproot_cross_check() {
    let Ok(python) = std::env::var("RHIZOID_ORACLE_PYTHON") else {
        return;
    };
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cross");
    let dir = TempDir::new("oracle");

    // uproot writes, we read and check every record of every codec.
    let out = std::process::Command::new(&python)
        .arg(scripts.join("uproot_make.py"))
        .arg(&dir.0)
        .output()
        .expect("run uproot_make.py");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for codec in ["zlib", "lz4", "zstd", "lzma", "none"] {
        let file = File::open(dir.path(&format!("uproot_{codec}.root"))).expect("open uproot file");
        let names: Vec<String> = file.keys().map(|k| k.name().to_owned()).collect();
        assert_eq!(names, ["hist", "dir", "events"], "{codec}");
        let sub = file.root().subdir("dir/sub").expect("subdir");
        assert_eq!(
            sub.keys()
                .map(|k| k.class_name().to_owned())
                .collect::<Vec<_>>(),
            ["TH2D"]
        );
        let report = file.check().expect("check");
        assert!(report.is_ok(), "{codec}: {report:?}");
        if codec != "none" {
            assert!(report.compressed > 0, "{codec}: nothing was compressed");
        }
    }

    // We write, uproot reads.
    let ours = dir.path("ours.root");
    rhizoid_io::create(&ours, |_| Ok::<(), rhizoid_io::Error>(())).expect("write");
    let out = std::process::Command::new(&python)
        .arg(scripts.join("uproot_read.py"))
        .arg(&ours)
        .output()
        .expect("run uproot_read.py");
    assert!(
        out.status.success(),
        "uproot could not read our file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("\"keys\": []"), "{json}");
    assert!(json.contains("\"fVersion\": 62600"), "{json}");
}
