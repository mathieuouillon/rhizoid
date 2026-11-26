use rhizoid_io::RootObject;

#[derive(RootObject)]
struct Event {
    n: f64,
    #[root(count = n)]
    pt: Vec<f32>,
}

fn main() {}
