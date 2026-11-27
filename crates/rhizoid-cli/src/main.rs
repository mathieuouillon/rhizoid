//! The `rhizoid` command: inspect and check CERN ROOT files.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rhizoid_io::columnar::Column;
use rhizoid_io::tree::Branch;
use rhizoid_io::{Array, DynObject, File, Value, WriteOptions};

/// Inspect and check CERN ROOT files, local or over HTTP(S).
#[derive(Debug, Parser)]
#[command(name = "rhizoid", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the keys of a directory (`FILE` or `FILE:dir/sub`).
    Ls {
        /// File, optionally followed by `:` and a directory path.
        target: String,
        /// Long format: offset, sizes, compression ratio and date.
        #[arg(short, long)]
        long: bool,
        /// Recurse into subdirectories.
        #[arg(short, long)]
        recursive: bool,
    },
    /// Walk every record and decompress it; report anything wrong.
    Check {
        /// The file to check.
        file: PathBuf,
    },
    /// Print the file header.
    Header {
        /// The file to inspect.
        file: PathBuf,
    },
    /// Print an object, member by member (`FILE:path/to/object`).
    Dump {
        /// File, `:`, and the object's path (optionally with `;cycle`).
        target: String,
        /// Print arrays in full instead of their first values.
        #[arg(short, long)]
        full: bool,
    },
    /// Describe a tree: branches, types, baskets, clusters (`FILE:tree`).
    Tree {
        /// File, `:`, and the tree's path.
        target: String,
    },
    /// Describe an `RNTuple`: fields, types, clusters (`FILE:ntuple`).
    Rntuple {
        /// File, `:`, and the `RNTuple`'s path.
        target: String,
    },
    /// Print entries of a tree or an `RNTuple` (`FILE:name`).
    Scan {
        /// File, `:`, and the tree's path.
        target: String,
        /// Branches to print (default: every readable top-level branch).
        #[arg(short, long, value_delimiter = ',')]
        branches: Vec<String>,
        /// First entry.
        #[arg(short, long, default_value_t = 0)]
        start: u64,
        /// Number of entries.
        #[arg(short = 'n', long, default_value_t = 10)]
        count: u64,
    },
    /// List the class descriptions stored in a file.
    Streamers {
        /// The file to inspect.
        file: PathBuf,
        /// Only this class.
        #[arg(short, long)]
        class: Option<String>,
    },
    /// Write an empty, valid ROOT file.
    New {
        /// Where to write it.
        file: PathBuf,
        /// Byte-identical output (ROOT's `kReproducible`).
        #[arg(long)]
        reproducible: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            let mut err = std::io::stderr().lock();
            let _ = writeln!(err, "rhizoid: {}", chain(&e));
            ExitCode::FAILURE
        }
    }
}

/// An error and its causes, `: `-separated, leaving out a cause whose text
/// the message before it already holds (rhizoid-io's I/O errors carry their
/// cause's text and the cause itself).
fn chain(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let text = cause.to_string();
        if out.contains(&text) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(": ");
        }
        out.push_str(&text);
    }
    out
}

/// Opens a local path or an `http(s)://` URL.
fn open_file(path: &std::path::Path) -> Result<File> {
    let s = path.to_string_lossy();
    let file = if s.starts_with("http://") || s.starts_with("https://") {
        File::open_url(&s)
    } else {
        File::open(path)
    };
    file.with_context(|| format!("cannot open {s}"))
}

fn split_target(target: &str) -> (PathBuf, String) {
    // Not separators: a Windows drive letter (`C:\…`), or a URL's scheme
    // and port (`https://host:8080/…`): look after the URL's host.
    let search_from = if let Some(rest) = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
    {
        let host_end = rest.find('/').unwrap_or(rest.len());
        target.len() - rest.len() + host_end
    } else if target.get(1..2) == Some(":") {
        2
    } else {
        0
    };
    match target.get(search_from..).and_then(|t| t.find(':')) {
        Some(i) => {
            let i = i + search_from;
            (PathBuf::from(&target[..i]), target[i + 1..].to_owned())
        }
        None => (PathBuf::from(target), String::new()),
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    match cli.command {
        Command::Ls {
            target,
            long,
            recursive,
        } => {
            let (path, dir) = split_target(&target);
            let file = open_file(&path)?;
            for d in file.diagnostics() {
                writeln!(std::io::stderr().lock(), "warning: {d}")?;
            }
            let root = file.root().subdir(&dir)?;
            list(&mut out, &root, long, recursive, 0)?;
        }
        Command::Check { file: path } => {
            let file = open_file(&path)?;
            for d in file.diagnostics() {
                writeln!(out, "warning: {d}")?;
            }
            let report = file.check()?;
            writeln!(
                out,
                "{}: {} records ({} compressed), {} gaps ({} bytes), {} free segments",
                path.display(),
                report.records,
                report.compressed,
                report.gaps,
                report.gap_bytes,
                report.free_segments
            )?;
            for (at, e) in &report.errors {
                writeln!(out, "error at {at}: {e}")?;
            }
            if let Some(why) = &report.stopped {
                writeln!(out, "walk stopped: {why}")?;
            }
            if !report.is_ok() {
                return Ok(ExitCode::FAILURE);
            }
            writeln!(out, "OK")?;
        }
        Command::Header { file: path } => {
            let file = open_file(&path)?;
            let h = file.header();
            writeln!(out, "name         {}", file.name())?;
            writeln!(out, "title        {}", file.title())?;
            writeln!(out, "version      {}", h.version())?;
            writeln!(out, "large        {}", h.is_large())?;
            writeln!(out, "begin / end  {} / {}", h.begin(), h.end())?;
            writeln!(
                out,
                "StreamerInfo {} ({} bytes)",
                h.seek_info(),
                h.nbytes_info()
            )?;
            writeln!(out, "free list    {}", h.seek_free())?;
            writeln!(out, "compression  {}", h.compression_setting())?;
        }
        Command::Dump { target, full } => dump_object(&mut out, &target, full)?,
        Command::Tree { target } => show_tree(&mut out, &target)?,
        Command::Rntuple { target } => show_rntuple(&mut out, &target)?,
        Command::Scan {
            target,
            branches,
            start,
            count,
        } => scan(&mut out, &target, branches, start, count)?,
        Command::Streamers { file: path, class } => {
            let file = open_file(&path)?;
            for info in file.streamer_infos()? {
                if class.as_deref().is_none_or(|c| c == info.class_name()) {
                    writeln!(out, "{info}")?;
                }
            }
        }
        Command::New {
            file: path,
            reproducible,
        } => {
            rhizoid_io::create_with(
                &path,
                WriteOptions::default().reproducible(reproducible),
                |_f| Ok::<_, rhizoid_io::Error>(()),
            )?;
            writeln!(out, "wrote {}", path.display())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn list(
    out: &mut impl Write,
    dir: &rhizoid_io::Directory,
    long: bool,
    recursive: bool,
    depth: usize,
) -> Result<()> {
    let indent = "  ".repeat(depth);
    for k in dir.keys() {
        if long {
            let ratio = if k.stored_len() > 0 {
                f64::from(k.object_len()) / f64::from(k.stored_len())
            } else {
                0.0
            };
            writeln!(
                out,
                "{indent}{:<24} {:<16} {:>12} {:>10} {:>10} {:>5.2}x  {}  {}",
                format!("{};{}", k.name(), k.cycle()),
                k.class_name(),
                k.offset().to_string(),
                k.stored_len(),
                k.object_len(),
                ratio,
                k.datime(),
                k.title()
            )?;
        } else {
            writeln!(out, "{indent}{k}")?;
        }
        if recursive && k.is_directory() {
            let name = format!("{};{}", k.name(), k.cycle());
            let sub = dir.subdir(&name)?;
            list(out, &sub, long, recursive, depth + 1)?;
        }
    }
    Ok(())
}

fn dump_object(out: &mut impl Write, target: &str, full: bool) -> Result<()> {
    let (path, object) = split_target(target);
    anyhow::ensure!(!object.is_empty(), "give the object as FILE:path");
    let file = open_file(&path)?;
    let obj = file.get_dyn(&object)?;
    dump(out, &obj, full, 0)?;
    for d in file.diagnostics() {
        writeln!(std::io::stderr().lock(), "warning: {d}")?;
    }
    Ok(())
}

fn open_tree(target: &str) -> Result<rhizoid_io::Tree> {
    let (path, name) = split_target(target);
    anyhow::ensure!(!name.is_empty(), "give the tree as FILE:path");
    let file = open_file(&path)?;
    Ok(file.tree(&name)?)
}

fn show_tree(out: &mut impl Write, target: &str) -> Result<()> {
    let tree = open_tree(target)?;
    writeln!(
        out,
        "{} \"{}\": {} entries, {} clusters",
        tree.name(),
        tree.title(),
        tree.num_entries(),
        tree.clusters().len()
    )?;
    let header = [
        "branch", "type", "entries", "baskets", "bytes", "on disk", "title",
    ];
    writeln!(out, "{}", branch_row(header.map(str::to_owned)))?;
    for b in tree.branches() {
        describe(out, b, 0)?;
    }
    Ok(())
}

fn describe_field(
    out: &mut impl Write,
    f: &rhizoid_io::ntuple::Field<'_>,
    depth: usize,
) -> Result<()> {
    let read_as = f
        .interpretation()
        .map_or_else(|e| format!("({e})"), |i| i.to_string());
    writeln!(
        out,
        "{:<32} {:<40} {}",
        format!("{}{}", "  ".repeat(depth), f.name()),
        f.type_name(),
        read_as
    )?;
    for sub in f.fields() {
        describe_field(out, &sub, depth + 1)?;
    }
    Ok(())
}

fn show_rntuple(out: &mut impl Write, target: &str) -> Result<()> {
    let (path, name) = split_target(target);
    anyhow::ensure!(!name.is_empty(), "give the RNTuple as FILE:path");
    let nt = open_file(&path)?.rntuple(&name)?;
    let version = nt.format_version().map(|v| v.to_string()).join(".");
    writeln!(
        out,
        "{} \"{}\": {} entries, {} clusters, format {version}, written by {}",
        nt.name(),
        nt.description(),
        nt.num_entries(),
        nt.clusters().len(),
        nt.written_by()
    )?;
    writeln!(out, "{:<32} {:<40} read as", "field", "type")?;
    for f in nt.fields() {
        describe_field(out, &f, 0)?;
    }
    Ok(())
}

/// Reads one column of a tree or an `RNTuple` over a range of entries.
type Reader = Box<dyn Fn(&str, std::ops::Range<u64>) -> rhizoid_io::Result<Column>>;

fn scan(
    out: &mut impl Write,
    target: &str,
    branches: Vec<String>,
    start: u64,
    count: u64,
) -> Result<()> {
    let (path, name) = split_target(target);
    anyhow::ensure!(!name.is_empty(), "give the tree or RNTuple as FILE:path");
    let file = open_file(&path)?;
    let is_ntuple = file.keys().any(|k| {
        k.name() == name.split(';').next().unwrap_or_default() && k.class_name() == "ROOT::RNTuple"
    });
    let (total, names, read): (u64, Vec<String>, Reader) = if is_ntuple {
        let nt = file.rntuple(&name)?;
        let names = if branches.is_empty() {
            nt.fields()
                .iter()
                .filter(|f| f.interpretation().is_ok())
                .map(|f| f.name().to_owned())
                .collect()
        } else {
            branches
        };
        (nt.num_entries(), names, Box::new(move |n, r| nt.read(n, r)))
    } else {
        let tree = file.tree(&name)?;
        let names = if branches.is_empty() {
            tree.readable_branches()
                .iter()
                .map(|b| b.name().to_owned())
                .collect()
        } else {
            branches
        };
        (
            tree.num_entries(),
            names,
            Box::new(move |n, r| tree.read(n, r)),
        )
    };
    let end = start.saturating_add(count).min(total);
    let start = start.min(end);
    let columns: Vec<Column> = names
        .iter()
        .map(|n| read(n, start..end))
        .collect::<rhizoid_io::Result<_>>()?;
    writeln!(out, "{:>8}  {}", "entry", names.join("  "))?;
    for (row, entry) in (start..end).enumerate() {
        let cells: Vec<String> = columns.iter().map(|c| cell(c, row)).collect();
        writeln!(out, "{entry:>8}  {}", cells.join("  "))?;
    }
    Ok(())
}

fn branch_row([name, ty, entries, baskets, bytes, disk, title]: [String; 7]) -> String {
    format!("{name:<24} {ty:<16} {entries:>9} {baskets:>8} {bytes:>10} {disk:>10}  {title}")
}

fn describe(out: &mut impl Write, b: &Branch, depth: usize) -> Result<()> {
    let ty = match b.interpretation() {
        Ok(i) => i.to_string(),
        Err(_) => format!("({})", b.class_name()),
    };
    let row = branch_row([
        format!("{}{}", "  ".repeat(depth), b.name()),
        ty,
        b.num_entries().to_string(),
        b.num_baskets().to_string(),
        b.uncompressed_bytes().to_string(),
        b.compressed_bytes().to_string(),
        b.title().to_owned(),
    ]);
    writeln!(out, "{row}")?;
    for sub in b.branches() {
        describe(out, sub, depth + 1)?;
    }
    Ok(())
}

fn values(a: &Array, range: std::ops::Range<usize>) -> String {
    let items: Vec<String> = range
        .filter_map(|i| a.get(i))
        .map(|v| v.to_string())
        .collect();
    format!("[{}]", items.join(", "))
}

/// A value, compactly: objects as `{member: value, …}`, lists and arrays
/// up to 8 items.
fn show(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut parts: Vec<String> = o
                .bases()
                .iter()
                .filter(|b| b.class() != "TObject")
                .map(|b| show(&Value::Object(Box::new(b.clone()))))
                .collect();
            parts.extend(o.members().map(|(n, m)| format!("{n}: {}", show(m))));
            format!("{{{}}}", parts.join(", "))
        }
        Value::List(items) => {
            let mut shown: Vec<String> = items.iter().take(8).map(show).collect();
            if items.len() > 8 {
                shown.push(format!("… {} more", items.len() - 8));
            }
            format!("[{}]", shown.join(", "))
        }
        Value::Array(a) => values(a, 0..a.len().min(8)),
        v => v.to_string(),
    }
}

/// One entry of a column, as text.
fn cell(c: &Column, i: usize) -> String {
    match c {
        Column::Values(a) => a.get(i).map(|v| v.to_string()).unwrap_or_default(),
        Column::Fixed { len, values: a } => values(a, i * len..(i + 1) * len),
        Column::Jagged { offsets, values: a } => match (offsets.get(i), offsets.get(i + 1)) {
            (Some(&lo), Some(&hi)) => values(a, lo..hi),
            _ => String::new(),
        },
        Column::Strings(s) => s.get(i).map(|s| format!("{s:?}")).unwrap_or_default(),
        Column::Objects(v) => v.get(i).map(show).unwrap_or_default(),
        _ => "?".to_owned(),
    }
}

/// Prints an object as an indented tree: bases first, then members.
fn dump(out: &mut impl Write, obj: &DynObject, full: bool, depth: usize) -> Result<()> {
    let indent = "  ".repeat(depth);
    if depth == 0 {
        writeln!(out, "{} (version {})", obj.class(), obj.version())?;
    }
    for base in obj.bases() {
        writeln!(out, "{indent}  [{} v{}]", base.class(), base.version())?;
        dump(out, base, full, depth + 1)?;
    }
    for (name, value) in obj.members() {
        match value {
            Value::Object(o) => {
                writeln!(
                    out,
                    "{indent}  {name}: {} (version {})",
                    o.class(),
                    o.version()
                )?;
                dump(out, o, full, depth + 1)?;
            }
            Value::List(items) => {
                writeln!(out, "{indent}  {name}: list of {}", items.len())?;
                for (i, item) in items.iter().enumerate() {
                    match item {
                        Value::Object(o) => {
                            writeln!(
                                out,
                                "{indent}    [{i}] {} (version {})",
                                o.class(),
                                o.version()
                            )?;
                            dump(out, o, full, depth + 2)?;
                        }
                        v => writeln!(out, "{indent}    [{i}] {v}")?,
                    }
                }
            }
            Value::Array(a) if full => {
                let values: Vec<String> = (0..a.len())
                    .filter_map(|i| a.get_f64(i))
                    .map(|v| v.to_string())
                    .collect();
                writeln!(
                    out,
                    "{indent}  {name}: [{}; {}] [{}]",
                    a.type_name(),
                    a.len(),
                    values.join(", ")
                )?;
            }
            v => writeln!(out, "{indent}  {name}: {v}")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_split_on_colon_but_not_drive_letters() {
        assert_eq!(
            split_target("f.root"),
            (PathBuf::from("f.root"), String::new())
        );
        assert_eq!(
            split_target("f.root:a/b"),
            (PathBuf::from("f.root"), "a/b".into())
        );
        assert_eq!(
            split_target("https://h.org:8080/d/f.root:dir/t"),
            (PathBuf::from("https://h.org:8080/d/f.root"), "dir/t".into())
        );
        assert_eq!(
            split_target("https://h.org/f.root"),
            (PathBuf::from("https://h.org/f.root"), String::new())
        );
        assert_eq!(
            split_target(r"C:\x\f.root:dir"),
            (PathBuf::from(r"C:\x\f.root"), "dir".into())
        );
    }
}
