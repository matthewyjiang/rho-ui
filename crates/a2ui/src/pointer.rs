use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Segments that no path may contain, in every language, so that one payload
/// behaves the same everywhere.
const FORBIDDEN_SEGMENTS: [&str; 3] = ["__proto__", "constructor", "prototype"];

/// A normalized absolute JSON Pointer into a data model.
///
/// Parsing splits on `/` and drops empty segments, so `//a///b//`, `/a/b/`
/// and `/a/b` are equal, and `""` and `/` are the root. Segments are stored
/// decoded: `~1` is `/` and `~0` is `~`, in that order (RFC 6901).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pointer {
    segments: Vec<String>,
}

/// Why a string is not a [`Pointer`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PointerError {
    /// A segment is `__proto__`, `constructor` or `prototype`.
    #[error("forbidden path segment {segment:?} in {path:?}")]
    ForbiddenSegment {
        /// The path that was being parsed.
        path: String,
        /// The decoded segment.
        segment: String,
    },
}

impl Pointer {
    /// The root pointer, `/`.
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    /// Parses `path` as a normalized absolute pointer.
    ///
    /// # Errors
    ///
    /// Returns [`PointerError::ForbiddenSegment`] when a segment is
    /// `__proto__`, `constructor` or `prototype`.
    pub fn parse(path: &str) -> Result<Self, PointerError> {
        let mut segments = Vec::new();
        for raw in path.split('/').filter(|raw| !raw.is_empty()) {
            let segment = raw.replace("~1", "/").replace("~0", "~");
            if FORBIDDEN_SEGMENTS.contains(&segment.as_str()) {
                return Err(PointerError::ForbiddenSegment {
                    path: path.to_owned(),
                    segment,
                });
            }
            segments.push(segment);
        }
        Ok(Self { segments })
    }

    /// The decoded segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Whether this is the root pointer.
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// Whether `self` is `other` or a path above it.
    #[must_use]
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        other.segments.starts_with(&self.segments)
    }

    /// The first `len` segments of `self`, which are already checked.
    pub(crate) fn truncated(&self, len: usize) -> Self {
        Self {
            segments: self.segments[..len].to_vec(),
        }
    }
}

impl FromStr for Pointer {
    type Err = PointerError;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Self::parse(path)
    }
}

impl fmt::Display for Pointer {
    /// Writes the canonical form with escapes, and `/` for the root.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.segments.is_empty() {
            return f.write_str("/");
        }
        for segment in &self.segments {
            write!(f, "/{}", segment.replace('~', "~0").replace('/', "~1"))?;
        }
        Ok(())
    }
}

/// Resolves a data-binding path against the path of its scope.
///
/// An absolute `path` ignores `context`. A relative one is joined to it, with
/// trailing slashes on the context dropped. An empty path or `.` is the
/// context itself, and no context is the root.
#[must_use]
pub fn resolve_path(path: &str, context: Option<&str>) -> String {
    if path.starts_with('/') {
        return path.to_owned();
    }
    let base = context.unwrap_or("").trim_end_matches('/');
    match path {
        "" | "." if base.is_empty() => "/".to_owned(),
        "" | "." => base.to_owned(),
        _ => format!("{base}/{path}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_escapes_and_round_trips() {
        let pointer = Pointer::parse("/a~1b/c~0d/~01").expect("valid pointer");
        assert_eq!(pointer.segments(), ["a/b", "c~d", "~1"]);
        assert_eq!(pointer.to_string(), "/a~1b/c~0d/~01");
        assert_eq!(Pointer::parse(&pointer.to_string()), Ok(pointer));
        assert_eq!(Pointer::root().to_string(), "/");
    }

    #[test]
    fn forbidden_segments_are_whole_segments() {
        assert!(Pointer::parse("/a/__proto__").is_err());
        assert!(Pointer::parse("/constructors").is_ok());
    }

    #[test]
    fn prefix_includes_equal_and_root() {
        let ab = Pointer::parse("/a/b").expect("valid pointer");
        assert!(Pointer::root().is_prefix_of(&ab));
        assert!(ab.is_prefix_of(&ab));
        assert!(!Pointer::parse("/a/bc").expect("valid").is_prefix_of(&ab));
    }
}
