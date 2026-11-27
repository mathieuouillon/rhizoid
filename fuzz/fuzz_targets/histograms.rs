//! The histogram API: axes and histograms built from arbitrary numbers
//! either refuse them with an error or, once written, read back exactly.
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rhizoid_fuzz::Shared;
use rhizoid_io::objects::{Axis, Hist1, Hist2, Histogram};
use rhizoid_io::{File, ReadOptions, WritableFile, WriteOptions};

#[derive(Arbitrary, Debug)]
enum AxisSpec {
    Uniform { bins: u16, min: f64, max: f64 },
    Variable(Vec<f64>),
}

#[derive(Arbitrary, Debug)]
struct Input {
    name: String,
    title: String,
    x: AxisSpec,
    y: AxisSpec,
    values: Vec<f64>,
    two_d: bool,
}

/// Up to 1024 bins an axis: a histogram's size is the caller's to choose,
/// and its memory with it.
fn axis(a: &AxisSpec) -> Option<Axis> {
    match a {
        AxisSpec::Uniform { bins, min, max } => {
            Axis::uniform(u32::from(*bins % 1024), *min, *max).ok()
        }
        AxisSpec::Variable(edges) if edges.len() <= 1025 => Axis::variable(edges).ok(),
        AxisSpec::Variable(_) => None,
    }
}

fn bits(v: &[f64]) -> Vec<u64> {
    v.iter().map(|x| x.to_bits()).collect()
}

/// The axes as the model exposes them. (`==` on axes also compares what an
/// axis read from a file keeps of its original object, which a new axis
/// does not have.)
fn same_axes(a: &[Axis], b: &[Axis]) {
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(b) {
        assert_eq!(a.nbins(), b.nbins());
        assert_eq!(a.is_variable(), b.is_variable());
        assert_eq!(bits(&a.edges()), bits(&b.edges()));
        assert_eq!(
            (a.min().to_bits(), a.max().to_bits()),
            (b.min().to_bits(), b.max().to_bits())
        );
        assert_eq!(a.labels(), b.labels());
    }
}

fuzz_target!(|input: Input| {
    let Some(x) = axis(&input.x) else { return };
    let sink = Shared::default();
    let mut file = WritableFile::to_sink(
        Box::new(sink.clone()),
        "fuzz.root",
        WriteOptions::default().reproducible(true),
    )
    .expect("create");
    if input.two_d {
        let Some(y) = axis(&input.y) else { return };
        #[allow(clippy::cast_possible_truncation)]
        let cells: Vec<f32> = input.values.iter().map(|v| *v as f32).collect();
        let Ok(h) = Hist2::<f32>::new(input.name.clone(), input.title.clone(), x, y)
            .with_values_flow(cells)
        else {
            return;
        };
        if file.put(&input.name, &h).is_err() {
            return;
        }
        file.close().expect("close");
        let file = File::from_source(sink.bytes(), ReadOptions::default()).expect("open");
        let back: Hist2<f32> = file.get(&input.name).expect("read back");
        assert_eq!(bits(&back.values_flow_f64()), bits(&h.values_flow_f64()));
        same_axes(back.axes(), h.axes());
    } else {
        let Ok(h) = Hist1::<f64>::new(input.name.clone(), input.title.clone(), x)
            .with_values_flow(input.values.clone())
        else {
            return;
        };
        if file.put(&input.name, &h).is_err() {
            return;
        }
        file.close().expect("close");
        let file = File::from_source(sink.bytes(), ReadOptions::default()).expect("open");
        let back: Hist1<f64> = file.get(&input.name).expect("read back");
        assert_eq!(bits(back.values_flow()), bits(h.values_flow()));
        assert_eq!(bits(&back.edges()), bits(&h.edges()));
        same_axes(back.axes(), h.axes());
    }
});
