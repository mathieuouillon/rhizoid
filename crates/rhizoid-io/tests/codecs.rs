//! Every compression setting ROOT writes, read from one file ROOT made
//! with each (`root_codecs.root`, from `tests/cross/root_codecs.C`): an
//! object and a tree per setting — zlib (101, 109), LZMA (207), ROOT's
//! old algorithm (306, raw deflate tagged `CS`), LZ4 (404), Zstandard
//! (505) and none. And every setting we write, read back by us; ROOT and
//! uproot read them in `tree_write.rs`.

use rhizoid_io::columnar::Column;
use rhizoid_io::objects::Named;
use rhizoid_io::{Algorithm, Array, Compression, File, WriteOptions};
use std::path::{Path, PathBuf};

const CODECS: [&str; 7] = ["zlib", "zlib9", "lzma", "old", "lz4", "zstd", "none"];
const ENTRIES: usize = 2000;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// The title `root_codecs.C` gives every object.
fn title() -> String {
    use std::fmt::Write as _;
    (0..1000).fold(String::new(), |mut s, i| {
        let _ = write!(s, "{},", (i * 7) % 101);
        s
    })
}

fn x() -> Column {
    Column::Values(Array::F64(
        (0..ENTRIES)
            .map(|i| f64::from(u32::try_from(i).expect("small")) * 0.25)
            .collect(),
    ))
}

fn k() -> Column {
    Column::Values(Array::I32(
        (0..ENTRIES)
            .map(|i| i32::try_from(i % 13).expect("small"))
            .collect(),
    ))
}

#[test]
fn every_codec_root_writes_is_read() {
    let file = File::open(data("root_codecs.root")).expect("open");
    for codec in CODECS {
        let named: Named = file.get(codec).expect(codec);
        assert_eq!(named.title(), title(), "{codec}");
        let tree = file.tree(&format!("t_{codec}")).expect(codec);
        assert_eq!(tree.read("x", ..).expect(codec), x(), "{codec}");
        assert_eq!(tree.read("k", ..).expect(codec), k(), "{codec}");
    }
    assert!(file.check().expect("check").is_ok());
}

#[derive(rhizoid_io::Row, Clone, Debug, PartialEq)]
struct Row {
    x: f64,
    k: i32,
}

#[test]
fn every_codec_we_write_reads_back() {
    let dir = std::env::temp_dir().join(format!("rhizoid-codecs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let rows: Vec<Row> = (0..ENTRIES)
        .map(|i| Row {
            x: f64::from(u32::try_from(i).expect("small")) * 0.25,
            k: i32::try_from(i % 13).expect("small"),
        })
        .collect();
    for setting in [
        Compression::zlib::<1>(),
        Compression::lz4::<4>(),
        Compression::zstd::<5>(),
        Compression::lzma::<7>(),
        Compression::new(Algorithm::OldRoot, 6).expect("old"),
        Compression::NONE,
    ] {
        let path = dir.join(format!("w{}.root", setting.setting()));
        rhizoid_io::create_with(&path, WriteOptions::default().compression(setting), |f| {
            f.put("n", &Named::new("n", title()))?;
            let mut t = f.tree::<Row>("t")?;
            t.extend(&rows)?;
            t.finish()
        })
        .expect("write");
        let file = File::open(&path).expect("open");
        assert_eq!(
            file.header().compression_setting(),
            setting.setting(),
            "{setting:?}"
        );
        assert_eq!(file.get::<Named>("n").expect("n").title(), title());
        let tree = file.tree("t").expect("tree");
        assert_eq!(tree.read("x", ..).expect("x"), x(), "{setting:?}");
        assert_eq!(tree.read("k", ..).expect("k"), k(), "{setting:?}");
        assert!(file.check().expect("check").is_ok());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Records added to a Zstandard file are compressed with Zstandard, as
/// ROOT does (they were compressed with zlib).
#[test]
fn updates_keep_the_file_s_codec() {
    let dir = std::env::temp_dir().join(format!("rhizoid-codecs-up-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("zstd.root");
    let blocks = |tag: &[u8; 3]| {
        let bytes = std::fs::read(&path).expect("read");
        bytes.windows(3).filter(|w| w == tag).count()
    };
    rhizoid_io::create_with(
        &path,
        WriteOptions::default().compression(Compression::zstd::<5>()),
        |f| f.put("a", &Named::new("a", title())),
    )
    .expect("write");
    let before = blocks(b"ZS\x01");
    rhizoid_io::update(&path, |f| f.put("b", &Named::new("b", title()))).expect("update");
    assert!(blocks(b"ZS\x01") > before, "the new record is Zstandard");
    assert_eq!(blocks(b"ZL\x08"), 0, "no zlib record");
    let file = File::open(&path).expect("open");
    assert_eq!(file.get::<Named>("b").expect("b").title(), title());
    let _ = std::fs::remove_dir_all(&dir);
}
