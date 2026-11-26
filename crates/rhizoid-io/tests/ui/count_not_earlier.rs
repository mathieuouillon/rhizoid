use rhizoid_io::RootObject;

#[derive(RootObject)]
struct Event {
    #[root(count = n)]
    pt: Vec<f32>,
    n: i32,
}

fn main() {}
