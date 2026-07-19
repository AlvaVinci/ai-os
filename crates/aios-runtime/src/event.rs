use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use aios_core::{DenialReason, TaskState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DEFAULT_MAX_EVENTS_PER_TASK: usize = 10_000;

/// Globally unique identifier for one task instance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TaskId(Uuid);

impl TaskId {
    /// Generates a time-ordered UUIDv7 task identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for TaskId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl FromStr for TaskId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Metadata recorded for a task lifecycle change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskEvent {
    pub task_id: TaskId,
    pub sequence: u64,
    pub occurred_at_unix_ms: u64,
    pub kind: TaskEventKind,
}

impl TaskEvent {
    /// Creates an event using the current wall-clock time.
    pub fn now(
        task_id: TaskId,
        sequence: u64,
        kind: TaskEventKind,
    ) -> Result<Self, EventStoreError> {
        if sequence == 0 {
            return Err(EventStoreError::SequenceExhausted);
        }

        Ok(Self {
            task_id,
            sequence,
            occurred_at_unix_ms: unix_time_ms(),
            kind,
        })
    }
}

/// Audit-safe event payloads. Task goals and capability values are excluded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TaskEventKind {
    Submitted,
    StateTransitioned {
        from: TaskState,
        to: TaskState,
    },
    ValidationFailed {
        error_count: usize,
    },
    OperationAllowed,
    OperationDenied {
        reason: DenialReason,
    },
    ApprovalRequested {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
    ApprovalGranted {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
    ApprovalDenied {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
    ApprovalExpired {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
    ApprovalRevoked {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
    ApprovalConsumed {
        approval_id: crate::ApprovalId,
        operation_id: crate::OperationId,
    },
}

/// Event persistence failure without leaking backend details to API callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventStoreError {
    CapacityExceeded,
    SequenceExhausted,
    Corrupt,
    InsecurePermissions,
    Unavailable,
}

impl Display for EventStoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::CapacityExceeded => "event capacity exceeded",
            Self::SequenceExhausted => "event sequence exhausted",
            Self::Corrupt => "event store data is corrupt",
            Self::InsecurePermissions => "event store permissions are insecure",
            Self::Unavailable => "event store unavailable",
        };
        formatter.write_str(message)
    }
}

impl Error for EventStoreError {}

/// Append-only storage used by the task supervisor.
pub trait EventStore {
    /// Atomically appends all event kinds for one task.
    fn append_batch(
        &mut self,
        task_id: TaskId,
        kinds: &[TaskEventKind],
    ) -> Result<Vec<TaskEvent>, EventStoreError>;

    /// Returns events with a sequence greater than `after_sequence`.
    fn list(&self, task_id: TaskId, after_sequence: u64)
    -> Result<Vec<TaskEvent>, EventStoreError>;
}

/// Bounded, process-local event store for early runtime development and tests.
#[derive(Debug)]
pub struct InMemoryEventStore {
    events: BTreeMap<TaskId, Vec<TaskEvent>>,
    max_events_per_task: usize,
}

impl InMemoryEventStore {
    /// Creates a store with a strict per-task event limit.
    pub fn new(max_events_per_task: usize) -> Result<Self, EventStoreError> {
        if max_events_per_task == 0 {
            return Err(EventStoreError::CapacityExceeded);
        }

        Ok(Self {
            events: BTreeMap::new(),
            max_events_per_task,
        })
    }
}

impl Default for InMemoryEventStore {
    fn default() -> Self {
        Self {
            events: BTreeMap::new(),
            max_events_per_task: DEFAULT_MAX_EVENTS_PER_TASK,
        }
    }
}

impl EventStore for InMemoryEventStore {
    fn append_batch(
        &mut self,
        task_id: TaskId,
        kinds: &[TaskEventKind],
    ) -> Result<Vec<TaskEvent>, EventStoreError> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        let existing_length = self.events.get(&task_id).map_or(0, Vec::len);
        let resulting_length = existing_length
            .checked_add(kinds.len())
            .ok_or(EventStoreError::CapacityExceeded)?;
        if resulting_length > self.max_events_per_task {
            return Err(EventStoreError::CapacityExceeded);
        }

        let first_sequence = u64::try_from(existing_length)
            .map_err(|_| EventStoreError::SequenceExhausted)?
            .checked_add(1)
            .ok_or(EventStoreError::SequenceExhausted)?;
        let mut appended = Vec::with_capacity(kinds.len());

        for (offset, kind) in kinds.iter().enumerate() {
            let offset = u64::try_from(offset).map_err(|_| EventStoreError::SequenceExhausted)?;
            let sequence = first_sequence
                .checked_add(offset)
                .ok_or(EventStoreError::SequenceExhausted)?;
            appended.push(TaskEvent::now(task_id, sequence, kind.clone())?);
        }

        self.events
            .entry(task_id)
            .or_default()
            .extend(appended.iter().cloned());
        Ok(appended)
    }

    fn list(
        &self,
        task_id: TaskId,
        after_sequence: u64,
    ) -> Result<Vec<TaskEvent>, EventStoreError> {
        Ok(self.events.get(&task_id).map_or_else(Vec::new, |events| {
            events
                .iter()
                .filter(|event| event.sequence > after_sequence)
                .cloned()
                .collect()
        }))
    }
}

fn unix_time_ms() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(milliseconds).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use aios_core::TaskState;

    use super::{EventStore, EventStoreError, InMemoryEventStore, TaskEventKind, TaskId};

    #[test]
    fn assigns_monotonic_sequences_per_task() {
        let mut store = InMemoryEventStore::default();
        let task_id = TaskId::new();

        store
            .append_batch(
                task_id,
                &[
                    TaskEventKind::Submitted,
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Submitted,
                        to: TaskState::Validating,
                    },
                ],
            )
            .expect("events should append");
        store
            .append_batch(
                task_id,
                &[TaskEventKind::StateTransitioned {
                    from: TaskState::Validating,
                    to: TaskState::Queued,
                }],
            )
            .expect("event should append");

        let events = store.list(task_id, 0).expect("events should list");
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(store.list(task_id, 2).expect("events should list").len(), 1);
    }

    #[test]
    fn rejects_a_batch_atomically_when_capacity_would_be_exceeded() {
        let mut store = InMemoryEventStore::new(2).expect("positive capacity");
        let task_id = TaskId::new();

        let error = store
            .append_batch(
                task_id,
                &[
                    TaskEventKind::Submitted,
                    TaskEventKind::ValidationFailed { error_count: 1 },
                    TaskEventKind::StateTransitioned {
                        from: TaskState::Validating,
                        to: TaskState::Rejected,
                    },
                ],
            )
            .expect_err("oversized batch must fail");

        assert_eq!(error, EventStoreError::CapacityExceeded);
        assert!(
            store
                .list(task_id, 0)
                .expect("events should list")
                .is_empty()
        );
    }
}
