//! Typed diff failures shared by the library, CLI trace, and exit policy.
//!
//! A diff failure always carries a contextual message and an explicit outcome
//! class. The message is what the CLI prints; the class is what the trace and
//! the exit policy consume, so neither has to infer intent from wording.
//!
//! There is deliberately no conversion from an untyped string or from an
//! arbitrary error. Every construction site must name a class, which keeps a
//! new failure mode from silently becoming a backend failure.

use std::fmt;

use pdfdelta_core::source::{ExtractionIssue, ExtractionIssueKind};

/// Outcome class of one failed diff operation.
///
/// The labels are the stable vocabulary of the flow trace's `error.kind`
/// field. [`Self::Backend`] covers fatal engine, I/O, configuration, and report
/// failures; the other classes preserve the distinctions the project requires
/// for unsupported input, unresolved content, and finite resource limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Fatal engine, I/O, configuration, or report failure.
    Backend,
    /// The input uses a feature this engine cannot inspect.
    Unsupported,
    /// Visible content could not be resolved into comparable evidence.
    Unresolved,
    /// A finite resource limit stopped the operation.
    LimitExceeded,
}

impl ErrorClass {
    /// Stable trace label for this class.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Backend => "backend",
            Self::Unsupported => "unsupported",
            Self::Unresolved => "unresolved",
            Self::LimitExceeded => "limit_exceeded",
        }
    }
}

/// The finite resource and configured bound of a limit failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// Stable name of the limited resource.
    pub resource: &'static str,
    /// Configured maximum for the resource.
    pub limit: usize,
}

/// A contextual diff failure with an explicit outcome class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    class: ErrorClass,
    message: String,
    limit: Option<Limit>,
}

impl Error {
    /// Creates a fatal engine, I/O, configuration, or report failure.
    #[must_use]
    pub fn backend(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Backend, message, None)
    }

    /// Creates an unsupported-input failure.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Unsupported, message, None)
    }

    /// Creates an unresolved-content failure.
    #[must_use]
    pub fn unresolved(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Unresolved, message, None)
    }

    /// Creates a resource-limit failure naming the resource and its bound.
    #[must_use]
    pub fn limit_exceeded(
        resource: &'static str,
        limit: usize,
        message: impl Into<String>,
    ) -> Self {
        Self::new(
            ErrorClass::LimitExceeded,
            message,
            Some(Limit { resource, limit }),
        )
    }

    /// Creates a resource-limit failure whose producer did not retain the
    /// configured bound.
    ///
    /// These still belong to [`ErrorClass::LimitExceeded`]; they are not
    /// backend failures. Prefer [`Self::limit_exceeded`] whenever the resource
    /// name and bound are known.
    #[must_use]
    pub fn limit_exceeded_without_bound(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::LimitExceeded, message, None)
    }

    /// Converts one extraction issue, taking the class from its typed kind.
    ///
    /// The description stays the human-readable message. This is the only
    /// place an extraction issue becomes a diff error, so the unsupported and
    /// unresolved distinction is carried structurally rather than by wording.
    #[must_use]
    pub fn from_extraction_issue(issue: &ExtractionIssue) -> Self {
        let class = match issue.kind() {
            ExtractionIssueKind::Unsupported => ErrorClass::Unsupported,
            ExtractionIssueKind::Unresolved => ErrorClass::Unresolved,
        };
        Self::new(class, issue.description(), None)
    }

    /// Prefixes the message with caller context while keeping the class.
    ///
    /// The resulting message is `"{context}: {message}"`, which matches the
    /// contextual strings the CLI has always printed.
    #[must_use]
    pub fn with_context(mut self, context: impl fmt::Display) -> Self {
        self.message = format!("{context}: {}", self.message);
        self
    }

    /// Appends caller context while keeping the class.
    ///
    /// The resulting message is `"{message}; {suffix}"`, which matches existing
    /// diagnostics that end by naming the incomplete output.
    #[must_use]
    pub fn with_suffix(mut self, suffix: impl fmt::Display) -> Self {
        self.message = format!("{}; {suffix}", self.message);
        self
    }

    /// The explicit outcome class.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        self.class
    }

    /// The contextual message printed by the CLI.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The limited resource and bound, for a limit-class failure.
    #[must_use]
    pub const fn limit(&self) -> Option<Limit> {
        self.limit
    }

    fn new(class: ErrorClass, message: impl Into<String>, limit: Option<Limit>) -> Self {
        Self {
            class,
            message: message.into(),
            limit,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<pdfdelta_core::Error> for Error {
    fn from(error: pdfdelta_core::Error) -> Self {
        let message = error.to_string();
        match error {
            pdfdelta_core::Error::Backend(_)
            | pdfdelta_core::Error::Report(_)
            | pdfdelta_core::Error::InvalidConfiguration(_) => Self::backend(message),
            pdfdelta_core::Error::Unsupported(_) => Self::unsupported(message),
            pdfdelta_core::Error::Unresolved(_) => Self::unresolved(message),
            pdfdelta_core::Error::LimitExceeded { resource, limit } => {
                Self::limit_exceeded(resource, limit, message)
            }
            // `pdfdelta_core::Error` is non-exhaustive. A new core variant is
            // fatal for the diff path until it receives an explicit class
            // here; this arm never inspects the message text.
            _ => Self::backend(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use pdfdelta_core::source::{ExtractionIssue, ExtractionScope};

    use super::{Error, ErrorClass};

    #[test]
    fn every_core_error_variant_keeps_its_class_and_message() {
        let cases = [
            (
                pdfdelta_core::Error::Backend("disk".to_owned()),
                ErrorClass::Backend,
            ),
            (
                pdfdelta_core::Error::Report("write".to_owned()),
                ErrorClass::Backend,
            ),
            (
                pdfdelta_core::Error::InvalidConfiguration("scale".to_owned()),
                ErrorClass::Backend,
            ),
            (
                pdfdelta_core::Error::Unsupported("security".to_owned()),
                ErrorClass::Unsupported,
            ),
            (
                pdfdelta_core::Error::Unresolved("paint".to_owned()),
                ErrorClass::Unresolved,
            ),
            (
                pdfdelta_core::Error::LimitExceeded {
                    resource: "glyphs",
                    limit: 7,
                },
                ErrorClass::LimitExceeded,
            ),
        ];
        for (error, expected) in cases {
            let message = error.to_string();
            let diff_error = Error::from(error);
            assert_eq!(diff_error.class(), expected);
            assert_eq!(diff_error.message(), message);
        }
    }

    #[test]
    fn limit_details_survive_context() {
        let error = Error::from(pdfdelta_core::Error::LimitExceeded {
            resource: "glyphs",
            limit: 7,
        })
        .with_context("failed to extract old.pdf");
        assert_eq!(error.class(), ErrorClass::LimitExceeded);
        assert_eq!(
            error.limit(),
            Some(super::Limit {
                resource: "glyphs",
                limit: 7
            })
        );
        assert!(error.message().starts_with("failed to extract old.pdf: "));
    }

    #[test]
    fn extraction_issues_keep_their_typed_kind() {
        let unsupported = ExtractionIssue::new(
            pdfdelta_core::source::ExtractionIssueKind::Unsupported,
            ExtractionScope::Document,
            "encrypted content was not examined",
        )
        .expect("issue");
        let unresolved = ExtractionIssue::new(
            pdfdelta_core::source::ExtractionIssueKind::Unresolved,
            ExtractionScope::Document,
            "unsupported wording in a description",
        )
        .expect("issue");
        assert_eq!(
            Error::from_extraction_issue(&unsupported).class(),
            ErrorClass::Unsupported
        );
        assert_eq!(
            Error::from_extraction_issue(&unresolved).class(),
            ErrorClass::Unresolved
        );
    }

    #[test]
    fn unbounded_limit_and_suffix_keep_their_class() {
        let error = Error::limit_exceeded_without_bound("worker response too large")
            .with_suffix("review directory /tmp/x is incomplete");
        assert_eq!(error.class(), ErrorClass::LimitExceeded);
        assert_eq!(error.limit(), None);
        assert_eq!(
            error.message(),
            "worker response too large; review directory /tmp/x is incomplete"
        );
    }

    #[test]
    fn classes_have_distinct_trace_labels() {
        let labels = [
            ErrorClass::Backend.label(),
            ErrorClass::Unsupported.label(),
            ErrorClass::Unresolved.label(),
            ErrorClass::LimitExceeded.label(),
        ];
        assert_eq!(
            labels,
            ["backend", "unsupported", "unresolved", "limit_exceeded"]
        );
    }
}
