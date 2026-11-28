//! Members on the decoder's less common paths, as ROOT writes them
//! (`tests/cross/root_members.C`, the fixture `root_members.root`).

use std::path::Path;

use rhizoid_io::columnar::Column;
use rhizoid_io::{DiagnosticKind, DynObject, ErrorKind, File, Value, WritableFile};

fn fixture() -> File {
    File::open(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/root_members.root"))
        .expect("open")
}

fn object<'a>(o: &'a DynObject, member: &str) -> &'a DynObject {
    o.get(member)
        .and_then(Value::as_object)
        .unwrap_or_else(|| unreachable!("{member}: not an object"))
}

fn list<'a>(o: &'a DynObject, member: &str) -> &'a [Value] {
    o.get(member)
        .and_then(Value::as_list)
        .unwrap_or_else(|| unreachable!("{member}: not a list"))
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("fString")
            .or_else(|| o.get("fName"))
            .and_then(Value::as_str)
            .expect("a string or a named object")
            .to_owned(),
        other => unreachable!("not text: {other:?}"),
    }
}

/// A `[key, value]` entry of a map.
fn entry(v: &Value) -> (&Value, &Value) {
    match v.as_list() {
        Some([k, v]) => (k, v),
        other => unreachable!("not a map entry: {other:?}"),
    }
}

#[test]
fn members_read_as_root_wrote_them() {
    let file = fixture();
    let m = file.get_dyn("members").expect("members");

    // An object-wise std::map: the size, then key, value, key, value.
    let map: Vec<(i64, f64)> = list(&m, "fMap")
        .iter()
        .map(entry)
        .map(|(k, v)| (k.as_i64().expect("int"), v.as_f64().expect("double")))
        .collect();
    assert_eq!(map, [(1, 0.5), (2, -1.25), (7, 3.0)]);

    // std::bitset<8>("10100101"): one byte per bit, the lowest first.
    let flags = m.get("fFlags").and_then(Value::as_array).expect("bits");
    let flags: Vec<bool> = (0..flags.len())
        .map(|i| flags.get(i) == Some(Value::Bool(true)))
        .collect();
    assert_eq!(flags, [true, false, true, false, false, true, false, true]);

    // The TRef carries the target's unique ID; the target's TObject, being
    // referenced, carries a process-ID index too, which must be skipped.
    let target = object(&m, "fTarget");
    assert_eq!(target.get("fName").and_then(Value::as_str), Some("target"));
    assert_eq!(
        target.get("fTitle").and_then(Value::as_str),
        Some("the referenced object")
    );
    let r = object(&m, "fRef");
    assert_eq!(r.class(), "TRef");
    assert_eq!(r.get("fUniqueID"), target.get("fUniqueID"));
    assert_eq!(r.get("fUniqueID"), Some(&Value::U32(1)));
    assert_eq!(r.get("fPID"), Some(&Value::U16(0)));

    // A TMap: key and value pointers. Its keys define class tags that
    // later members refer to.
    let table = object(&m, "fTable");
    assert_eq!(table.class(), "TMap");
    assert_eq!(table.base("TObject").map(DynObject::class), Some("TObject"));
    let mut pairs: Vec<(String, String)> = list(table, "fObjects")
        .iter()
        .map(entry)
        .map(|(k, v)| (text(k), text(v)))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        [
            ("key".to_owned(), "value".to_owned()),
            ("other".to_owned(), "named".to_owned())
        ]
    );

    // Kept undecoded, whole, with their headers: a TBits member, a pointer
    // to one, and a std::forward_list.
    for (member, class, len) in [
        ("fMask", "TBits", 29),
        ("fAny", "TBits", 27),
        ("fForward", "forward_list<int>", 22),
    ] {
        match m.get(member) {
            Some(Value::Opaque { class: c, bytes }) => {
                assert_eq!((c.as_str(), bytes.len()), (class, len), "{member}");
            }
            other => unreachable!("{member}: {other:?}"),
        }
    }

    // TLists with options, and without (no fOptions then).
    let tagged = object(&m, "fTagged");
    let names: Vec<String> = list(tagged, "fObjects").iter().map(text).collect();
    assert_eq!(names, ["a", "b"]);
    let options: Vec<String> = list(tagged, "fOptions").iter().map(text).collect();
    assert_eq!(options, ["first", "second"]);
    let plain = object(&m, "fPlain");
    let names: Vec<String> = list(plain, "fObjects").iter().map(text).collect();
    assert_eq!(names, ["c", "d"]);
    assert_eq!(plain.get("fOptions"), None);

    let array = object(&m, "fArray");
    assert_eq!(array.get("fName").and_then(Value::as_str), Some("array"));
    assert_eq!(array.get("fLowerBound"), Some(&Value::I32(0)));
    let names: Vec<String> = list(array, "fObjects").iter().map(text).collect();
    assert_eq!(names, ["e"]);

    // Packed floats: Double32_t[-1,1,16] and Float16_t[0,10,12].
    assert_eq!(m.get("fRanged"), Some(&Value::F64(0.5)));
    assert_eq!(m.get("fHalf"), Some(&Value::F32(2.5)));
    assert_eq!(m.get("fLabel").and_then(Value::as_str), Some("members"));

    // Each member kept undecoded is noted where its object ends, in order.
    let diagnostics = file.diagnostics();
    assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
    let mut previous = 100;
    for (d, what) in diagnostics
        .iter()
        .zip(["class TBits", "class TBits", "a forward_list<int>"])
    {
        assert_eq!(d.kind(), DiagnosticKind::Irregular);
        assert!(d.message().contains(what), "{d:?}");
        let at = d.offset().expect("an offset").get();
        assert!(at > previous, "{d:?}");
        previous = at;
    }
}

/// Classes read by hand that cannot be written yet are refused, though the
/// file describes them.
#[test]
fn tref_and_tmap_are_read_not_written() {
    let file = fixture();
    let m = file.get_dyn("members").expect("members");
    let path = std::env::temp_dir().join(format!("rhizoid-members-{}.root", std::process::id()));
    let mut out = WritableFile::create(&path).expect("create");
    for member in ["fRef", "fTable"] {
        let e = out.put("x", object(&m, member)).expect_err(member);
        assert_eq!(e.kind(), ErrorKind::Unsupported, "{e}");
        assert!(e.to_string().contains("is not supported yet"), "{e}");
    }
    drop(out);
    std::fs::remove_file(path).ok();
}

/// Unsplit maps in a tree, streamed member-wise: all keys, then all values.
#[test]
fn unsplit_map_branches_read_member_wise() {
    let file = fixture();
    let t = file.tree("t").expect("tree");
    let entries = |name: &str| match t.read(name, ..).expect(name) {
        Column::Objects(v) => v,
        other => unreachable!("{name}: {other:?}"),
    };
    let ms: Vec<Vec<(i64, String)>> = entries("ms")
        .iter()
        .map(|e| {
            e.as_list()
                .expect("a map")
                .iter()
                .map(entry)
                .map(|(k, v)| (k.as_i64().expect("int"), text(v)))
                .collect()
        })
        .collect();
    let expected: Vec<Vec<(i64, String)>> = (0..3)
        .map(|i| {
            (0..=i)
                .map(|k| (10 * i + k, format!("s{}", 10 * i + k)))
                .collect()
        })
        .collect();
    assert_eq!(ms, expected);

    let mv: Vec<Vec<(String, Vec<f64>)>> = entries("mv")
        .iter()
        .map(|e| {
            e.as_list()
                .expect("a map")
                .iter()
                .map(entry)
                .map(|(k, v)| {
                    let a = v.as_array().expect("floats");
                    let v = (0..a.len())
                        .map(|i| a.get(i).and_then(|x| x.as_f64()).expect("a float"))
                        .collect();
                    (text(k), v)
                })
                .collect()
        })
        .collect();
    let expected: Vec<Vec<(String, Vec<f64>)>> = (0..3)
        .map(|i| {
            (0..=i)
                .map(|k| {
                    let v = (0..k).map(|j| f64::from(i) + 0.5 * f64::from(j)).collect();
                    (format!("k{k}"), v)
                })
                .collect()
        })
        .collect();
    assert_eq!(mv, expected);
}
