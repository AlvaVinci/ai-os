use std::error::Error;
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::TaskState;

/// Stable error categories exposed by the runtime API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidTask,
    IdempotencyConflict,
    CapabilityDenied,
    ApprovalExpired,
    BudgetExceeded,
    RuntimeUnavailable,
    InternalError,
}

/// A validation failure associated with one input field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    field: &'static str,
    message: String,
}

impl ValidationError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }

    /// Returns the stable API error code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::InvalidTask
    }

    /// Returns the field that failed validation.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    /// Returns a human-readable explanation without including input values.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for ValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

/// All validation failures found in a task specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    pub(crate) fn new(errors: Vec<ValidationError>) -> Option<Self> {
        if errors.is_empty() {
            None
        } else {
            Some(Self { errors })
        }
    }

    /// Returns validation failures in deterministic field order.
    #[must_use]
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }
}

impl Display for ValidationErrors {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "task validation failed with {} error(s)",
            self.errors.len()
        )
    }
}

impl Error for ValidationErrors {}

/// An attempted state transition that is not part of the task lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateTransitionError {
    from: TaskState,
    to: TaskState,
}

impl StateTransitionError {
    pub(crate) const fn new(from: TaskState, to: TaskState) -> Self {
        Self { from, to }
    }

    #[must_use]
    pub const fn from(&self) -> TaskState {
        self.from
    }

    #[must_use]
    pub const fn to(&self) -> TaskState {
        self.to
    }
}

impl Display for StateTransitionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid task state transition: {:?} -> {:?}",
            self.from, self.to
        )
    }
}

impl Error for StateTransitionError {}
