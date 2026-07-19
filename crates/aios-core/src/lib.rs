//! Trusted domain types for the AI OS runtime.
//!
//! This crate does not execute models or tools. It defines and validates the
//! task contract that an execution layer must satisfy before doing work.

mod error;
mod policy;
mod state;
mod task;

pub use error::{ErrorCode, StateTransitionError, ValidationError, ValidationErrors};
pub use policy::{CapabilityPolicy, CapabilityRequest, DenialReason, PolicyDecision};
pub use state::TaskState;
pub use task::{
    ApprovalPolicy, Budget, CapabilitySet, FileAccess, FileCapability, NetworkDestination,
    NetworkPolicy, NetworkTransport, TaskSpec,
};
