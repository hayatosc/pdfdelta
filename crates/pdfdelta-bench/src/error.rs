use std::fmt;

pub type Result<T> = std::result::Result<T, BenchError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BenchError {
    InvalidInput(String),
    Publication(String),
    Render {
        renderer: &'static str,
        message: String,
    },
    Core {
        stage: &'static str,
        source: pdfdelta_core::Error,
    },
}

impl BenchError {
    /// Wraps a core failure with the benchmark stage that requested it.
    pub(crate) fn core(stage: &'static str, source: pdfdelta_core::Error) -> Self {
        Self::Core { stage, source }
    }
}

impl fmt::Display for BenchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(formatter, "invalid benchmark input: {message}"),
            Self::Publication(message) => write!(formatter, "publication failed: {message}"),
            Self::Render { renderer, message } => {
                write!(formatter, "{renderer} renderer failed: {message}")
            }
            Self::Core { stage, source } => {
                write!(formatter, "core {stage} failed: {source}")
            }
        }
    }
}

impl std::error::Error for BenchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core { source, .. } => Some(source),
            Self::InvalidInput(_) | Self::Publication(_) | Self::Render { .. } => None,
        }
    }
}
