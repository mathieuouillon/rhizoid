use rhizoid_io::RootObject;

#[derive(RootObject)]
struct Event {
    run: std::collections::HashMap<i32, i32>,
}

fn main() {}
