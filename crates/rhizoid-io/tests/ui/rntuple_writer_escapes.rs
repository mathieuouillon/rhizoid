#[derive(rhizoid_io::Row)]
struct Event {
    run: i32,
}

fn main() {
    // The writer borrows the file: it cannot outlive `create`'s scope.
    let w = rhizoid_io::create("o.root", |f| f.rntuple::<Event>("t"));
    let _ = w;
}
