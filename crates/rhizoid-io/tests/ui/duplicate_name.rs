use rhizoid_io::RootObject;

#[derive(RootObject)]
struct Event {
    #[root(rename = "fX")]
    x: i32,
    #[root(rename = "fX")]
    y: i32,
}

fn main() {}
