//! Stateful task supervision for AI OS.
//!
//! The runtime records an audit event before applying each in-memory state
//! change. It does not execute models, tools, or operating-system operations.

mod approval;
mod event;
mod supervisor;

pub use approval::{
    ApprovalAuthority, ApprovalError, ApprovalGrant, ApprovalId, ApprovalReceipt, ApprovalRequest,
    OperationId,
};
pub use event::{
    EventStore, EventStoreError, InMemoryEventStore, TaskEvent, TaskEventKind, TaskId,
};
pub use supervisor::{
    OperationAuthorization, SubmitResult, SupervisorError, TaskSnapshot, TaskSupervisor,
};
