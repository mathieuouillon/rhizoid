#[derive(rhizoid_io::Row)]
struct Event {
    run: i32,
}

#[derive(rhizoid_io::Row)]
struct Run {
    number: i32,
}

fn main() -> rhizoid_io::Result<()> {
    rhizoid_io::create("o.root", |f| {
        let mut events = f.tree::<Event>("t")?;
        events.fill(&Run { number: 1 })
    })
}
