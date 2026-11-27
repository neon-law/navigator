//! Shared workflow failures, independent of the durable execution engine.

/// Whether retrying a failed shared step can recover without changing its input.
#[derive(Debug, thiserror::Error)]
pub enum WorkflowStepError {
    #[error(transparent)]
    Retryable(#[from] anyhow::Error),
    #[error("{0}")]
    Terminal(String),
}

impl WorkflowStepError {
    #[must_use]
    pub fn retryable(error: impl Into<anyhow::Error>) -> Self {
        Self::Retryable(error.into())
    }

    #[must_use]
    pub fn terminal(message: impl Into<String>) -> Self {
        Self::Terminal(message.into())
    }
}
