#[derive(rhizoid_io::Row)]
struct Event {
    run: i32,
}

fn main() -> rhizoid_io::Result<()> {
    rhizoid_io::create("o.root", |f| {
        let mut w = f.rntuple::<Event>("t")?;
        w.finish()?;
        w.fill(&Event { run: 1 })
    })
}
