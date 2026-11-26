//! The error type shared by every layer, in the style of `std::io::Error`.

use core::fmt;
use std::error::Error as StdError;

use rhizoid_source::FileOffset;

/// What went wrong, for matching. Fieldless and `Copy`, like `io::ErrorKind`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// An I/O error of the underlying source or sink.
    Io,
    /// A length or offset points past the end of the available bytes.
    UnexpectedEof,
    /// The bytes do not start with the `root` magic.
    NotRootFile,
    /// A structure on disk is inconsistent.
    Malformed,
    /// A version of a structure this library does not understand.
    UnsupportedVersion,
    /// A compression block with an unknown algorithm tag.
    UnknownCompression,
    /// A compressed block failed to decompress.
    Decompress,
    /// A value read from the file exceeds a configured [`ReadLimits`](crate::ReadLimits) bound.
    LimitExceeded,
    /// No object at the requested path.
    NotFound,
    /// A malformed object path.
    InvalidPath,
    /// The operation is not supported (yet, or by this source/sink).
    Unsupported,
    /// Strict mode turned a tolerated irregularity into an error.
    Strict,
    /// The object is not of the class requested.
    ClassMismatch,
    /// A branch's values are not of the requested type.
    TypeMismatch,
    /// A value given to a writer does not fit its class description.
    InvalidInput,
    /// An error raised by user code through [`Error::custom`].
    Other,
}

impl ErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Io => "i/o error",
            Self::UnexpectedEof => "unexpected end of data",
            Self::NotRootFile => "not a ROOT file",
            Self::Malformed => "malformed data",
            Self::UnsupportedVersion => "unsupported version",
            Self::UnknownCompression => "unknown compression algorithm",
            Self::Decompress => "decompression failed",
            Self::LimitExceeded => "limit exceeded",
            Self::NotFound => "not found",
            Self::InvalidPath => "invalid object path",
            Self::Unsupported => "unsupported",
            Self::Strict => "rejected by strict mode",
            Self::ClassMismatch => "class mismatch",
            Self::TypeMismatch => "type mismatch",
            Self::InvalidInput => "invalid input",
            Self::Other => "error",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where in the file an error happened. Gathered as the error bubbles up.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Context {
    /// A byte offset in the file.
    Offset(FileOffset),
    /// The key (object path) being read.
    Key(String),
    /// The class being decoded.
    Class(String),
}

/// The error type of `rhizoid-io`: one pointer wide, opaque, with a
/// [`kind`](Error::kind) to match on and context to read.
pub struct Error(Box<Inner>);

struct Inner {
    kind: ErrorKind,
    message: String,
    context: Vec<Context>,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl Error {
    /// An error of `kind` with a message, like `std::io::Error::new`; for
    /// adapter crates that report rhizoid errors.
    #[cold]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self(Box::new(Inner {
            kind,
            message: message.into(),
            context: Vec::new(),
            source: None,
        }))
    }

    /// Creates an error from user code, for example a hand-written decoder.
    #[cold]
    pub fn custom(message: impl fmt::Display) -> Self {
        Self::new(ErrorKind::Other, message.to_string())
    }

    #[cold]
    pub(crate) fn malformed(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Malformed, message)
    }

    /// Returns what went wrong.
    pub fn kind(&self) -> ErrorKind {
        self.0.kind
    }

    /// Returns the innermost file offset recorded in the context, if any.
    pub fn offset(&self) -> Option<FileOffset> {
        self.0.context.iter().find_map(|c| match c {
            Context::Offset(o) => Some(*o),
            _ => None,
        })
    }

    /// Returns the object path recorded in the context, if any.
    pub fn object_path(&self) -> Option<&str> {
        self.0.context.iter().find_map(|c| match c {
            Context::Key(k) => Some(k.as_str()),
            _ => None,
        })
    }

    /// Returns the context, innermost first.
    pub fn context(&self) -> impl Iterator<Item = &Context> {
        self.0.context.iter()
    }

    #[must_use]
    pub(crate) fn with_context(mut self, context: Context) -> Self {
        self.0.context.push(context);
        self
    }

    #[must_use]
    pub(crate) fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.0.source = Some(Box::new(source));
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.message)?;
        for c in &self.0.context {
            match c {
                Context::Offset(o) => write!(f, " at offset {o}")?,
                Context::Class(c) => write!(f, " in class {c}")?,
                Context::Key(k) => write!(f, " in key `{k}`")?,
            }
        }
        Ok(())
    }
}

/// Written for `unwrap()` output: kind, message and context on separate lines.
impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} ({})", self.0.message, self.0.kind)?;
        for c in &self.0.context {
            match c {
                Context::Offset(o) => writeln!(f, "    at offset {o}")?,
                Context::Class(c) => writeln!(f, "    in class {c}")?,
                Context::Key(k) => writeln!(f, "    in key `{k}`")?,
            }
        }
        if let Some(s) = &self.0.source {
            writeln!(f, "    caused by: {s}")?;
        }
        Ok(())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0
            .source
            .as_deref()
            .map(|s| s as &(dyn StdError + 'static))
    }
}

impl From<std::io::Error> for Error {
    #[cold]
    fn from(e: std::io::Error) -> Self {
        let kind = if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ErrorKind::UnexpectedEof
        } else {
            ErrorKind::Io
        };
        Self::new(kind, e.to_string()).with_source(e)
    }
}

/// `Result` with [`Error`] as the default error type.
pub type Result<T, E = Error> = core::result::Result<T, E>;

/// Adds context to errors.
pub(crate) trait ResultExt<T> {
    fn context(self, context: Context) -> Result<T>;
}

impl<T> ResultExt<T> for Result<T> {
    fn context(self, context: Context) -> Result<T> {
        self.map_err(|e| e.with_context(context))
    }
}

/// A tolerated irregularity, reported instead of failing (lenient mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    kind: DiagnosticKind,
    message: String,
    offset: Option<FileOffset>,
}

/// The kind of a [`Diagnostic`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DiagnosticKind {
    /// The file was not closed or is truncated; its key list was rebuilt by
    /// walking the records, as ROOT's recovery does.
    Recovered,
    /// A structure was irregular but readable.
    Irregular,
}

impl Diagnostic {
    pub(crate) fn new(
        kind: DiagnosticKind,
        message: impl Into<String>,
        offset: Option<FileOffset>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            offset,
        }
    }

    /// Returns the kind of irregularity.
    pub fn kind(&self) -> DiagnosticKind {
        self.kind
    }

    /// Returns the human-readable description.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the file offset concerned, if known.
    pub fn offset(&self) -> Option<FileOffset> {
        self.offset
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(o) = self.offset {
            write!(f, " (at offset {o})")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_is_one_pointer_wide() {
        assert_eq!(size_of::<Error>(), size_of::<usize>());
    }

    #[test]
    fn display_includes_context_innermost_first() {
        let e = Error::new(
            ErrorKind::UnexpectedEof,
            "unexpected end of buffer (needed 8, had 3)",
        )
        .with_context(Context::Offset(FileOffset::new(0x1f3a0)))
        .with_context(Context::Class("TH1D".into()))
        .with_context(Context::Key("dir/hist;2".into()));
        assert_eq!(
            e.to_string(),
            "unexpected end of buffer (needed 8, had 3) at offset 0x1f3a0 in class TH1D in key `dir/hist;2`"
        );
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
        assert_eq!(e.offset(), Some(FileOffset::new(0x1f3a0)));
        assert_eq!(e.object_path(), Some("dir/hist;2"));
    }

    #[test]
    fn io_eof_maps_to_unexpected_eof() {
        let e: Error = std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into();
        assert_eq!(e.kind(), ErrorKind::UnexpectedEof);
        assert!(e.source().is_some());
    }
}
