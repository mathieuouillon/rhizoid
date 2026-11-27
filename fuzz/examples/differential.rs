//! The differential check's writer: decodes each input of the `write_rows`
//! corpus as the fuzz target does, writes its file, and records what every
//! reader must find in it. `differential/check.py` then has ROOT and uproot
//! read the files and compares.
//!
//! ```text
//! cargo run --release --manifest-path fuzz/Cargo.toml --example differential -- \
//!     fuzz/corpus/write_rows OUT_DIR [MAX_FILES]
//! ```
//!
//! For each input `i` it writes `OUT_DIR/i.root`, `i.expected` (the rows
//! as `rows::dump` prints them) and `i.kind` (`tree` or `rntuple`).

use std::path::PathBuf;
use std::process::ExitCode;

use arbitrary::{Arbitrary, Unstructured};
use rhizoid_fuzz::rows::{Input, dump, write};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(corpus), Some(out)) = (args.first(), args.get(1)) else {
        eprintln!("usage: differential CORPUS_DIR OUT_DIR [MAX_FILES]");
        return ExitCode::FAILURE;
    };
    let max: usize = args.get(2).and_then(|m| m.parse().ok()).unwrap_or(300);
    let out = PathBuf::from(out);
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("cannot create {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    let mut inputs: Vec<PathBuf> = match std::fs::read_dir(corpus) {
        Ok(dir) => dir.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(e) => {
            eprintln!("cannot read {corpus}: {e}");
            return ExitCode::FAILURE;
        }
    };
    inputs.sort();
    let mut written = 0;
    for path in inputs {
        if written == max {
            break;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // As libFuzzer hands bytes to the target.
        let Ok(input) = Input::arbitrary_take_rest(Unstructured::new(&bytes)) else {
            continue;
        };
        let Some((rows, file)) = write(&input) else {
            continue;
        };
        let stem = out.join(format!("{written:04}"));
        let kind = if input.rntuple { "rntuple" } else { "tree" };
        let ok = std::fs::write(stem.with_extension("root"), file).is_ok()
            && std::fs::write(stem.with_extension("expected"), dump(&rows)).is_ok()
            && std::fs::write(stem.with_extension("kind"), kind).is_ok();
        if !ok {
            eprintln!("cannot write {}", stem.display());
            return ExitCode::FAILURE;
        }
        written += 1;
    }
    println!("{written} files written to {}", out.display());
    ExitCode::SUCCESS
}
