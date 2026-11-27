# rhizoid-derive

The derive macros of [`rhizoid-io`](https://crates.io/crates/rhizoid-io),
re-exported by it (feature `derive`, on by default): use them through
`rhizoid_io`, not this crate.

- `#[derive(RootObject)]`: a user class with its own class description,
  checksum included as ROOT computes it, written with it and read back
  from any version of the class.
- `#[derive(Row)]`: a typed row of a `TTree` or an `RNTuple`.

```rust
use rhizoid_io::objects::{Named, Object};
use rhizoid_io::{RootObject, Row};

/// ROOT's `class Hit : public TObject { Int_t fId; Float_t fE[3]; ... }`.
#[derive(RootObject, Clone, Debug, PartialEq)]
#[root(class = "Hit", version = 2, rename_all = "root")]
struct Hit {
    #[root(base)]
    object: Object,
    id: i32,     // fId
    e: [f32; 3], // fE[3]
    #[root(alias = "fCharge")]
    q: f64,      // also read from its older name
}

/// A row of a tree: a branch per field.
#[derive(Row)]
struct Muons {
    #[row(rename = "nMuon")]
    n: u32,
    #[row(rename = "Muon_pt")]
    pt: Vec<f32>,
}
# let _ = (Named::new("n", "t"), std::any::type_name::<Hit>(), std::any::type_name::<Muons>());
```

Field attributes, mistakes and what each refuses are documented on the
derives in `rhizoid-io`. License: MIT.
