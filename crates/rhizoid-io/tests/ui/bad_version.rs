use rhizoid_io::RootObject;

#[derive(RootObject)]
#[root(version = 0)]
struct Event {
    run: i32,
}

fn main() {}
