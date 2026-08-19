use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Backend(String),
    Report(String),
    InvalidConfiguration(String),
    Unsupported(String),
    Unresolved(String),
    LimitExceeded {
        resource: &'static str,
        limit: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(message) => write!(formatter, "backend error: {message}"),
            Self::Report(message) => write!(formatter, "report error: {message}"),
            Self::InvalidConfiguration(message) => {
                write!(formatter, "invalid configuration: {message}")
            }
            Self::Unsupported(feature) => write!(formatter, "unsupported feature: {feature}"),
            Self::Unresolved(reason) => write!(formatter, "unresolved content: {reason}"),
            Self::LimitExceeded { resource, limit } => {
                write!(formatter, "{resource} exceeded its limit of {limit}")
            }
        }
    }
}

impl std::error::Error for Error {}
