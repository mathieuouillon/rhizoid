use rhizoid_io::RootObject;

#[derive(RootObject)]
#[root(clas = "Event")]
struct Event {
    #[root(renam = "fRun")]
    run: i32,
}

fn main() {}
