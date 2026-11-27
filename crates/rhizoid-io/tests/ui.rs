//! The compile-time guarantees of the derives and writers: each mistake
//! is refused with its own message. Compiler
//! messages change between releases, so this runs with `RHIZOID_UI=1` on
//! the stable toolchain CI uses (`TRYBUILD=overwrite` refreshes them).

#![cfg(feature = "derive")]

#[test]
fn derive_mistakes_do_not_compile() {
    if std::env::var_os("RHIZOID_UI").is_none() {
        return;
    }
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
