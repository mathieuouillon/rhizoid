//! The object models against ROOT itself: ROOT reads the objects we build
//! as we do, we read what ROOT writes as ROOT does, and ROOT reads its own
//! objects after we rewrite them, unchanged (fitted functions included).
//! ROOT must not warn or complain while reading. Every class modeled:
//! `TH1`/`TH2`/`TH3` in each cell type, profiles, graphs, `TF1`, `TNamed`,
//! `TObjString`.
//!
//! Without ROOT, the ROOT-written fixture (`tests/data/root_hists.root`) is
//! still compared with what ROOT read from it (`root_hists.txt`, made by
//! `tools/root-fixture.sh`). With `RHIZOID_ROOT` naming a `root`
//! executable, every direction runs against that ROOT.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rhizoid_io::objects::{
    Cell, ErrorMode, HistStats, TAxis, TF1, TGraph, TGraphAsymmErrors, TGraphErrors, TH1, TH2, TH3,
    TNamed, TObjString, TProfile, TProfile2D, TProfile3D,
};
use rhizoid_io::{File, FileScope};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn macro_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/cross")
        .join(name)
}

/// A fact: text, or floats compared exactly.
#[derive(Clone, Debug, PartialEq)]
enum Fact {
    Text(String),
    Floats(Vec<f64>),
}

/// `(key, field)` → fact.
type Dump = BTreeMap<(String, String), Fact>;

const TEXT_FIELDS: [&str; 11] = [
    "class",
    "name",
    "title",
    "string",
    "axistitle0",
    "axistitle1",
    "axistitle2",
    "functions",
    "errormode",
    "parnames",
    "formula",
];

/// Parses `KEY FIELD VALUE...` lines (from `root_dump.C`).
fn parse(text: &str) -> Dump {
    let mut out = Dump::new();
    for line in text.lines() {
        if line.starts_with("Processing") || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ' ');
        let (Some(key), Some(field)) = (parts.next(), parts.next()) else {
            continue;
        };
        let rest = parts.next().unwrap_or_default();
        let fact = if TEXT_FIELDS.contains(&field) || key == "FILE" {
            Fact::Text(rest.to_owned())
        } else {
            Fact::Floats(
                rest.split_whitespace()
                    .map(|x| x.parse().expect("a float"))
                    .collect(),
            )
        };
        out.insert((key.to_owned(), field.to_owned()), fact);
    }
    out
}

fn run_root(root: &str, script: &str, file: &Path) -> std::process::Output {
    std::process::Command::new(root)
        .args(["-b", "-q", "-l"])
        .arg(format!(
            "{}(\"{}\")",
            macro_path(script).display(),
            file.display()
        ))
        .output()
        .expect("run root")
}

/// What ROOT reads from `file`, checking it said nothing while reading.
fn root_dump(root: &str, file: &Path) -> Dump {
    let out = run_root(root, "root_dump.C", file);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "ROOT failed on {}: {stderr}",
        file.display()
    );
    for line in stdout.lines().chain(stderr.lines()) {
        assert!(
            !(line.contains("Error in <") || line.contains("Warning in <")),
            "ROOT complained reading {}: {line}",
            file.display()
        );
    }
    let dump = parse(&stdout);
    assert_eq!(
        dump.get(&("FILE".to_owned(), "recovered".to_owned())),
        Some(&Fact::Text("0".to_owned())),
        "ROOT had to recover {}",
        file.display()
    );
    dump
}

fn root() -> Option<String> {
    std::env::var("RHIZOID_ROOT").ok()
}

/// The 64-bit histograms (`TH1L`, `TH2L`, `TH3L`) exist from ROOT 6.32 on:
/// an older ROOT has no such class and cannot read them.
fn has_long_histograms(root: &str) -> bool {
    let out = std::process::Command::new(root)
        .args(["-b", "-q", "-l", "-e"])
        .arg(r#"printf("ROOTVERSION %d\n", gROOT->GetVersionInt());"#)
        .output()
        .expect("run root");
    let version: u32 = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("ROOTVERSION ")?.trim().parse().ok())
        .expect("ROOT's version");
    version >= 6_32_00
}

const LONG_HISTOGRAMS: [&str; 3] = ["TH1L", "TH2L", "TH3L"];

fn text(s: &str) -> Fact {
    Fact::Text(s.to_owned())
}

fn nums(v: &[f64]) -> Fact {
    Fact::Floats(v.to_vec())
}

/// `GetStats`'s order: Σw, Σw², Σwx, Σwx², then y, then z.
fn stats(s: &HistStats, dim: usize) -> Vec<f64> {
    let mut v = vec![s.tsumw, s.tsumw2, s.tsumwx, s.tsumwx2];
    if dim >= 2 {
        v.extend([s.tsumwy, s.tsumwy2, s.tsumwxy]);
    }
    if dim >= 3 {
        v.extend([s.tsumwz, s.tsumwz2, s.tsumwxz, s.tsumwyz]);
    }
    v
}

/// A sink for facts about one key.
struct Facts<'a>(&'a mut Dump, &'a str);

impl Facts<'_> {
    fn put(&mut self, field: &str, fact: Fact) {
        self.0.insert((self.1.to_owned(), field.to_owned()), fact);
    }

    fn axes(&mut self, axes: &[&TAxis]) {
        for (i, a) in axes.iter().enumerate() {
            self.put(&format!("edges{i}"), Fact::Floats(a.edges()));
            self.put(&format!("axistitle{i}"), text(a.title()));
        }
    }

    fn functions(&mut self, fs: &[TF1]) {
        let names: Vec<String> = fs.iter().map(|f| format!("TF1:{}", f.name())).collect();
        self.put("functions", Fact::Text(names.join(" ")));
    }
}

macro_rules! hist_facts {
    ($f:expr, $h:expr, $dim:expr, $s:expr, [$($axis:ident),*]) => {{
        let h = $h;
        $f.put("name", text(h.name()));
        $f.put("title", text(h.title()));
        $f.put("values", Fact::Floats(h.values_flow().iter().map(|v| v.to_f64()).collect()));
        $f.put("sumw2", nums(h.sumw2()));
        $f.put("fEntries", Fact::Floats(vec![h.entries()]));
        $f.put("stats", Fact::Floats($s));
        $f.axes(&[$(h.$axis()),*]);
        $f.functions(&h.functions());
    }};
}

macro_rules! profile_facts {
    ($f:expr, $h:expr, $dim:expr, [$($axis:ident),*]) => {{
        let h = $h;
        let mut s = stats(h.stats(), $dim);
        let (sum, sum2) = h.value_sums();
        s.extend([sum, sum2]);
        hist_facts!($f, h, $dim, s, [$($axis),*]);
        $f.put("binentries", nums(h.bin_entries()));
        $f.put("binsumw2", nums(h.bin_sumw2()));
        $f.put("errormode", text(h.error_mode().option()));
        let (lo, hi) = h.value_range();
        $f.put("range", Fact::Floats(vec![lo, hi]));
    }};
}

fn hist1<T: Cell>(f: &mut Facts<'_>, file: &File, key: &str) {
    let h: TH1<T> = file.get(key).expect("read");
    hist_facts!(f, &h, 1, stats(h.stats(), 1), [x_axis]);
}

fn hist2<T: Cell>(f: &mut Facts<'_>, file: &File, key: &str) {
    let h: TH2<T> = file.get(key).expect("read");
    hist_facts!(f, &h, 2, stats(h.stats(), 2), [x_axis, y_axis]);
}

fn hist3<T: Cell>(f: &mut Facts<'_>, file: &File, key: &str) {
    let h: TH3<T> = file.get(key).expect("read");
    hist_facts!(f, &h, 3, stats(h.stats(), 3), [x_axis, y_axis, z_axis]);
}

fn graph_points(
    f: &mut Facts<'_>,
    name: &str,
    title: &str,
    x: &[f64],
    y: &[f64],
    range: (f64, f64),
) {
    f.put("name", text(name));
    f.put("title", text(title));
    f.put("x", nums(x));
    f.put("y", nums(y));
    f.put("range", Fact::Floats(vec![range.0, range.1]));
}

/// What we read from `file`, in `root_dump.C`'s terms.
fn our_dump(file: &File) -> Dump {
    let mut out = Dump::new();
    let keys: Vec<(String, String)> = file
        .keys()
        .map(|k| {
            (
                format!("{};{}", k.name(), k.cycle()),
                k.class_name().to_owned(),
            )
        })
        .collect();
    for (key, class) in keys {
        let mut f = Facts(&mut out, &key);
        f.put("class", text(&class));
        match class.as_str() {
            "TObjString" => {
                let s: TObjString = file.get(&key).expect("read");
                f.put("string", text(s.as_str()));
            }
            "TNamed" => {
                let n: TNamed = file.get(&key).expect("read");
                f.put("name", text(n.name()));
                f.put("title", text(n.title()));
            }
            "TH1C" => hist1::<i8>(&mut f, file, &key),
            "TH1S" => hist1::<i16>(&mut f, file, &key),
            "TH1I" => hist1::<i32>(&mut f, file, &key),
            "TH1L" => hist1::<i64>(&mut f, file, &key),
            "TH1F" => hist1::<f32>(&mut f, file, &key),
            "TH1D" => hist1::<f64>(&mut f, file, &key),
            "TH2C" => hist2::<i8>(&mut f, file, &key),
            "TH2S" => hist2::<i16>(&mut f, file, &key),
            "TH2I" => hist2::<i32>(&mut f, file, &key),
            "TH2L" => hist2::<i64>(&mut f, file, &key),
            "TH2F" => hist2::<f32>(&mut f, file, &key),
            "TH2D" => hist2::<f64>(&mut f, file, &key),
            "TH3C" => hist3::<i8>(&mut f, file, &key),
            "TH3S" => hist3::<i16>(&mut f, file, &key),
            "TH3I" => hist3::<i32>(&mut f, file, &key),
            "TH3L" => hist3::<i64>(&mut f, file, &key),
            "TH3F" => hist3::<f32>(&mut f, file, &key),
            "TH3D" => hist3::<f64>(&mut f, file, &key),
            "TProfile" => {
                let p: TProfile = file.get(&key).expect("read");
                profile_facts!(f, &p, 1, [x_axis]);
            }
            "TProfile2D" => {
                let p: TProfile2D = file.get(&key).expect("read");
                profile_facts!(f, &p, 2, [x_axis, y_axis]);
            }
            "TProfile3D" => {
                let p: TProfile3D = file.get(&key).expect("read");
                profile_facts!(f, &p, 3, [x_axis, y_axis, z_axis]);
            }
            "TGraph" => {
                let g: TGraph = file.get(&key).expect("read");
                graph_points(&mut f, g.name(), g.title(), g.x(), g.y(), g.y_range());
            }
            "TGraphErrors" => {
                let g: TGraphErrors = file.get(&key).expect("read");
                graph_points(&mut f, g.name(), g.title(), g.x(), g.y(), g.y_range());
                f.put("ex", nums(g.ex()));
                f.put("ey", nums(g.ey()));
            }
            "TGraphAsymmErrors" => {
                let g: TGraphAsymmErrors = file.get(&key).expect("read");
                graph_points(&mut f, g.name(), g.title(), g.x(), g.y(), g.y_range());
                f.put("exlow", nums(g.ex_low()));
                f.put("exhigh", nums(g.ex_high()));
                f.put("eylow", nums(g.ey_low()));
                f.put("eyhigh", nums(g.ey_high()));
            }
            "TF1" => {
                let t: TF1 = file.get(&key).expect("read");
                f.put("name", text(t.name()));
                f.put("title", text(t.title()));
                f.put("parameters", nums(t.parameters()));
                f.put("parerrors", nums(t.par_errors()));
                f.put("parnames", Fact::Text(t.par_names().join(" ")));
                let (lo, hi) = t.domain();
                f.put("domain", Fact::Floats(vec![lo, hi]));
                f.put("fit", Fact::Floats(vec![t.chisquare(), f64::from(t.ndf())]));
                f.put("formula", text(t.formula()));
            }
            other => unreachable!("no model for {other}"),
        }
    }
    out
}

/// Every fact ROOT reports equals ours, and we report nothing ROOT does
/// not.
fn compare(label: &str, theirs: &Dump, ours: &Dump) {
    let mut differ = Vec::new();
    for ((key, field), fact) in theirs {
        if key == "FILE" {
            continue;
        }
        match ours.get(&(key.clone(), field.clone())) {
            Some(mine) if mine == fact => {}
            mine => differ.push(format!("{key} {field}: ROOT {fact:?}, rhizoid {mine:?}")),
        }
    }
    for k in ours.keys() {
        if !theirs.contains_key(k) {
            differ.push(format!("{} {}: only rhizoid reports it", k.0, k.1));
        }
    }
    assert!(differ.is_empty(), "{label}:\n{}", differ.join("\n"));
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("rhizoid-root-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&p).expect("tmp");
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// We read the ROOT-written fixture as ROOT read it (no ROOT needed).
#[test]
fn we_read_root_written_objects() {
    let theirs = parse(&std::fs::read_to_string(data("root_hists.txt")).expect("dump"));
    let file = File::open(data("root_hists.root")).expect("open");
    assert!(file.diagnostics().is_empty(), "{:?}", file.diagnostics());
    compare("root_hists.root", &theirs, &our_dump(&file));
}

/// A fresh file written by the ROOT at hand reads the same way.
#[test]
fn we_read_what_this_root_writes() {
    let Some(root) = root() else { return };
    let dir = TempDir::new("make");
    let path = dir.0.join("made.root");
    let made = run_root(&root, "root_make.C", &path);
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let file = File::open(&path).expect("open");
    compare("made by ROOT", &root_dump(&root, &path), &our_dump(&file));
}

/// A deterministic source of test values.
struct Values(u32);

impl Values {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        f64::from(self.0 % 10_000) / 100.0
    }

    fn vec(&mut self, n: usize) -> Vec<f64> {
        (0..n).map(|_| self.next()).collect()
    }

    #[expect(clippy::cast_possible_truncation, reason = "test values fit")]
    fn ints<T: TryFrom<i64>>(&mut self, n: usize) -> Vec<T> {
        (0..n)
            .map(|_| T::try_from(self.next() as i64 % 100).ok().expect("fits"))
            .collect()
    }

    fn stats(&mut self) -> HistStats {
        let mut s = HistStats::default();
        s.entries = 1000.0;
        for v in [
            &mut s.tsumw,
            &mut s.tsumw2,
            &mut s.tsumwx,
            &mut s.tsumwx2,
            &mut s.tsumwy,
            &mut s.tsumwy2,
            &mut s.tsumwxy,
            &mut s.tsumwz,
            &mut s.tsumwz2,
            &mut s.tsumwxz,
            &mut s.tsumwyz,
        ] {
            *v = self.next();
        }
        s
    }
}

fn axis(n: u32, lo: f64, hi: f64, title: &str) -> TAxis {
    TAxis::uniform(n, lo, hi).expect("axis").with_title(title)
}

/// Writes one object of every modeled class, built from data.
#[expect(
    clippy::too_many_lines,
    reason = "one object per class, read top to bottom"
)]
fn write_every_class(f: &FileScope<'_>, longs: bool) -> rhizoid_io::Result<()> {
    let mut v = Values(0x2545_f491);
    let var = || {
        TAxis::variable(&[0.0, 0.5, 2.0, 2.5])
            .expect("axis")
            .with_title("y")
    };

    macro_rules! h1 {
        ($t:ty, $name:literal, $cells:ident) => {{
            let mut h = TH1::<$t>::new($name, concat!("a ", $name), axis(4, -1.0, 1.0, "x"));
            h.set_values_flow(v.$cells(6))?;
            *h.stats_mut() = v.stats();
            f.put($name, &h)?;
        }};
    }
    h1!(i8, "h1c", ints);
    h1!(i16, "h1s", ints);
    h1!(i32, "h1i", ints);
    if longs {
        h1!(i64, "h1l", ints);
    }
    let mut h1f = TH1::<f32>::new("h1f", "a TH1F", var());
    #[expect(clippy::cast_possible_truncation, reason = "test values")]
    h1f.set_values_flow(v.vec(5).into_iter().map(|x| x as f32).collect())?;
    f.put("h1f", &h1f)?;
    let mut h1d = TH1::<f64>::new("h1d", "a TH1D", axis(4, -1.0, 1.0, "x [GeV]"));
    h1d.set_values_flow(v.vec(6))?;
    h1d.set_sumw2(v.vec(6))?;
    *h1d.stats_mut() = v.stats();
    f.put("h1d", &h1d)?;

    macro_rules! h2 {
        ($t:ty, $name:literal, $cells:ident) => {{
            let mut h = TH2::<$t>::new($name, $name, axis(3, 0.0, 3.0, "x"), var());
            h.set_values_flow(v.$cells(25))?;
            *h.stats_mut() = v.stats();
            f.put($name, &h)?;
        }};
    }
    h2!(i8, "h2c", ints);
    h2!(i16, "h2s", ints);
    h2!(i32, "h2i", ints);
    if longs {
        h2!(i64, "h2l", ints);
    }
    h2!(f64, "h2d", vec);
    let mut h2f = TH2::<f32>::new(
        "h2f",
        "a TH2F",
        axis(2, 0.0, 2.0, ""),
        axis(2, 0.0, 2.0, ""),
    );
    h2f.set_sumw2(v.vec(16))?;
    f.put("h2f", &h2f)?;

    macro_rules! h3 {
        ($t:ty, $name:literal, $cells:ident) => {{
            let mut h = TH3::<$t>::new(
                $name,
                $name,
                axis(2, 0.0, 2.0, "x"),
                axis(2, -1.0, 1.0, "y"),
                axis(1, 0.0, 5.0, "z"),
            );
            h.set_values_flow(v.$cells(48))?;
            *h.stats_mut() = v.stats();
            f.put($name, &h)?;
        }};
    }
    h3!(i8, "h3c", ints);
    h3!(i16, "h3s", ints);
    h3!(i32, "h3i", ints);
    if longs {
        h3!(i64, "h3l", ints);
    }
    h3!(f64, "h3d", vec);
    let h3f = TH3::<f32>::new(
        "h3f",
        "empty TH3F",
        axis(2, 0.0, 2.0, ""),
        axis(2, 0.0, 2.0, ""),
        axis(2, 0.0, 2.0, ""),
    );
    f.put("h3f", &h3f)?;

    let mut p1 = TProfile::new("p1", "a TProfile", axis(4, -2.0, 2.0, "x"));
    p1.set_values_flow(v.vec(6))?;
    p1.set_sumw2(v.vec(6))?;
    p1.set_bin_entries(v.vec(6))?;
    p1.set_bin_sumw2(v.vec(6))?;
    p1.set_error_mode(ErrorMode::Spread);
    p1.set_value_range(-10.0, 10.0);
    p1.set_value_sums(v.next(), v.next());
    *p1.stats_mut() = v.stats();
    f.put("p1", &p1)?;
    let mut p2 = TProfile2D::new("p2", "a TProfile2D", axis(2, 0.0, 2.0, "x"), var());
    p2.set_values_flow(v.vec(20))?;
    p2.set_sumw2(v.vec(20))?;
    p2.set_bin_entries(v.vec(20))?;
    p2.set_value_sums(v.next(), v.next());
    *p2.stats_mut() = v.stats();
    f.put("p2", &p2)?;
    let mut p3 = TProfile3D::new(
        "p3",
        "a TProfile3D",
        axis(1, 0.0, 1.0, ""),
        axis(1, 0.0, 1.0, ""),
        axis(1, 0.0, 1.0, ""),
    );
    p3.set_values_flow(v.vec(27))?;
    p3.set_sumw2(v.vec(27))?;
    p3.set_bin_entries(v.vec(27))?;
    p3.set_error_mode(ErrorMode::SpreadG);
    *p3.stats_mut() = v.stats();
    f.put("p3", &p3)?;

    let (x, y) = (v.vec(5), v.vec(5));
    f.put("g", &TGraph::new("g", "a TGraph", x.clone(), y.clone())?)?;
    f.put(
        "ge",
        &TGraphErrors::new(
            "ge",
            "a TGraphErrors",
            x.clone(),
            y.clone(),
            v.vec(5),
            v.vec(5),
        )?,
    )?;
    let mut ga = TGraphAsymmErrors::new(
        "ga",
        "a TGraphAsymmErrors",
        x,
        y,
        [v.vec(5), v.vec(5), v.vec(5), v.vec(5)],
    )?;
    ga.set_y_range(-5.0, 50.0);
    f.put("ga", &ga)?;
    f.put("named", &TNamed::new("named", "a TNamed"))?;
    f.put("s", &TObjString::new("written by rhizoid"))
}

/// ROOT reads every object we build as we do.
#[test]
fn root_reads_our_objects() {
    let Some(root) = root() else { return };
    let dir = TempDir::new("ours");
    let path = dir.0.join("ours.root");
    let longs = has_long_histograms(&root);
    rhizoid_io::create(&path, |f| write_every_class(f, longs)).expect("write");
    let theirs = root_dump(&root, &path);
    let ours = our_dump(&File::open(&path).expect("open"));
    compare("written by rhizoid", &theirs, &ours);
    assert_eq!(
        theirs.keys().filter(|(_, f)| f == "class").count(),
        if longs { 26 } else { 23 },
        "ROOT lists every object"
    );
}

/// Our objects read back as we wrote them, without ROOT.
#[test]
fn our_objects_read_back() {
    let dir = TempDir::new("back");
    let path = dir.0.join("back.root");
    rhizoid_io::create(&path, |f| write_every_class(f, true)).expect("write");
    let file = File::open(&path).expect("open");
    let dump = our_dump(&file);
    assert_eq!(dump.keys().filter(|(_, f)| f == "class").count(), 26);
    assert!(file.check().expect("check").is_ok());
}

/// ROOT's objects, read by us and written back, read in ROOT exactly as
/// before: the members the models do not expose (the fitted function, line
/// colours, …) are kept.
#[test]
fn root_reads_root_objects_we_rewrite() {
    let Some(root) = root() else { return };
    let original = File::open(data("root_hists.root")).expect("open");
    let dir = TempDir::new("rewrite");
    let path = dir.0.join("rewritten.root");
    let longs = has_long_histograms(&root);
    // What this ROOT cannot read, left out.
    let skipped: Vec<String> = original
        .keys()
        .filter(|k| !longs && LONG_HISTOGRAMS.contains(&k.class_name()))
        .map(|k| k.name().to_owned())
        .collect();
    rhizoid_io::create(&path, |f| {
        for k in original.keys() {
            let name = k.name().to_owned();
            if skipped.contains(&name) {
                continue;
            }
            macro_rules! copy {
                ($t:ty) => {
                    f.put(&name, &original.get::<$t>(&name)?)?
                };
            }
            match k.class_name() {
                "TH1C" => copy!(TH1<i8>),
                "TH1S" => copy!(TH1<i16>),
                "TH1I" => copy!(TH1<i32>),
                "TH1L" => copy!(TH1<i64>),
                "TH1F" => copy!(TH1<f32>),
                "TH1D" => copy!(TH1<f64>),
                "TH2C" => copy!(TH2<i8>),
                "TH2S" => copy!(TH2<i16>),
                "TH2I" => copy!(TH2<i32>),
                "TH2L" => copy!(TH2<i64>),
                "TH2F" => copy!(TH2<f32>),
                "TH2D" => copy!(TH2<f64>),
                "TH3C" => copy!(TH3<i8>),
                "TH3S" => copy!(TH3<i16>),
                "TH3I" => copy!(TH3<i32>),
                "TH3L" => copy!(TH3<i64>),
                "TH3F" => copy!(TH3<f32>),
                "TH3D" => copy!(TH3<f64>),
                "TProfile" => copy!(TProfile),
                "TProfile2D" => copy!(TProfile2D),
                "TProfile3D" => copy!(TProfile3D),
                "TGraph" => copy!(TGraph),
                "TGraphErrors" => copy!(TGraphErrors),
                "TGraphAsymmErrors" => copy!(TGraphAsymmErrors),
                "TF1" => copy!(TF1),
                "TNamed" => copy!(TNamed),
                "TObjString" => copy!(TObjString),
                other => unreachable!("no model for {other}"),
            }
        }
        Ok::<(), rhizoid_io::Error>(())
    })
    .expect("rewrite");
    let before = parse(&std::fs::read_to_string(data("root_hists.txt")).expect("dump"));
    let after = root_dump(&root, &path);
    let mut differ = Vec::new();
    // The dump names objects with their cycle: `h1l;1`.
    let kept = |key: &str| !skipped.iter().any(|s| key.split(';').next() == Some(s));
    for (k, fact) in before.iter().filter(|(k, _)| kept(&k.0)) {
        if after.get(k) != Some(fact) {
            differ.push(format!(
                "{} {}: before {fact:?}, after {:?}",
                k.0,
                k.1,
                after.get(k)
            ));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}
