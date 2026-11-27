//! Rows beyond flat numbers: user classes in trees, written as split
//! branches (an object, and a `std::vector` of objects), and every field
//! type an `RNTuple` or a tree writes (all integer widths, fixed arrays,
//! vectors of numbers and of booleans, strings). Both must read back
//! exactly.
#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use rhizoid_fuzz::Shared;
use rhizoid_io::objects::Object;
use rhizoid_io::{File, ReadOptions, RootObject, Row, WritableFile, WriteOptions};

/// A user class, split in trees as ROOT splits it.
#[derive(RootObject, Clone, Debug, Default)]
#[root(class = "Hit", version = 1, rename_all = "root")]
struct Hit {
    #[root(base)]
    object: Object,
    id: i32,
    e: [f32; 3],
}

impl<'a> Arbitrary<'a> for Hit {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        Ok(Self {
            object: Object::default(),
            id: u.arbitrary()?,
            e: u.arbitrary()?,
        })
    }
}

#[derive(Row, Arbitrary, Clone, Debug)]
struct Split {
    run: i32,
    #[row(rename = "hit.")]
    hit: Hit,
    hits: Vec<Hit>,
}

/// Every field type the writers take.
#[derive(Row, Arbitrary, Clone, Debug)]
struct Wide {
    a: i16,
    b: u16,
    c: u32,
    d: i64,
    e: u8,
    f: [f64; 2],
    g: bool,
    #[row(count = "ng")]
    h: Vec<i16>,
    #[row(count = "nu")]
    u: Vec<u64>,
    #[row(count = "nb")]
    flags: Vec<bool>,
    s: String,
}

#[derive(Arbitrary, Debug)]
struct Input {
    split: Vec<Split>,
    wide: Vec<Wide>,
    rntuple: bool,
}

fn hit_bits(h: &Hit) -> (i32, [u32; 3]) {
    (h.id, h.e.map(f32::to_bits))
}

fn wide_bits(w: &Wide) -> String {
    format!(
        "{} {} {} {} {} {:?} {} {:?} {:?} {:?} {:?}",
        w.a,
        w.b,
        w.c,
        w.d,
        w.e,
        w.f.map(f64::to_bits),
        w.g,
        w.h,
        w.u,
        w.flags,
        w.s
    )
}

fuzz_target!(|input: Input| {
    let split: Vec<Split> = input
        .split
        .into_iter()
        .take(64)
        .map(|mut s| {
            s.hits.truncate(8);
            s
        })
        .collect();
    let wide: Vec<Wide> = input.wide.into_iter().take(64).collect();
    let sink = Shared::default();
    let mut file = WritableFile::to_sink(
        Box::new(sink.clone()),
        "fuzz.root",
        WriteOptions::default().reproducible(true),
    )
    .expect("create");
    let mut t = file.tree::<Split>("split").expect("split tree");
    t.extend(&split).expect("fill");
    t.finish().expect("finish");
    if input.rntuple {
        let mut w = file.rntuple::<Wide>("wide").expect("rntuple");
        w.extend(&wide).expect("fill");
        w.finish().expect("finish");
    } else {
        let mut w = file.tree::<Wide>("wide").expect("tree");
        w.extend(&wide).expect("fill");
        w.finish().expect("finish");
    }
    file.close().expect("close");

    let file = File::from_source(sink.bytes(), ReadOptions::default()).expect("open");
    assert!(file.check().expect("check").is_ok());
    let back: Vec<Split> = file
        .tree("split")
        .expect("tree")
        .rows::<Split>()
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    assert_eq!(back.len(), split.len());
    for (a, b) in split.iter().zip(&back) {
        assert_eq!(a.run, b.run);
        assert_eq!(hit_bits(&a.hit), hit_bits(&b.hit));
        assert_eq!(
            a.hits.iter().map(hit_bits).collect::<Vec<_>>(),
            b.hits.iter().map(hit_bits).collect::<Vec<_>>()
        );
    }
    let back: Vec<Wide> = if input.rntuple {
        let nt = file.rntuple("wide").expect("rntuple");
        nt.rows::<Wide>()
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect()
    } else {
        let t = file.tree("wide").expect("tree");
        t.rows::<Wide>()
            .expect("rows")
            .map(|r| r.expect("row"))
            .collect()
    };
    assert_eq!(
        back.iter().map(wide_bits).collect::<Vec<_>>(),
        wide.iter().map(wide_bits).collect::<Vec<_>>()
    );
});
