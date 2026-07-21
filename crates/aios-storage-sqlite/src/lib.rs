//! SQLite-backed, append-only event persistence for AI OS.
//!
//! This crate persists audit-safe task events. It deliberately does not store
//! task goals, capability values, or other sensitive task input.

use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use aios_core::TaskState;
use aios_runtime::{
    EventStore, EventStoreError, RecoverableEventStore, TaskEvent, TaskEventKind, TaskId,
    TaskSnapshot,
};
use rusqlite::{Connection, TransactionBehavior, params};

const SCHEMA_VERSION: i64 = 1;
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const CREATE_SCHEMA: &str = "
    CREATE TABLE task_events (
        task_id TEXT NOT NULL,
        sequence INTEGER NOT NULL CHECK (sequence > 0),
        occurred_at_unix_ms INTEGER NOT NULL CHECK (occurred_at_unix_ms >= 0),
        event_json TEXT NOT NULL,
        PRIMARY KEY (task_id, sequence)
    ) WITHOUT ROWID;
";

/// Persistent event store backed by one SQLite database.
pub struct SqliteEventStore {
    connection: Connection,
    max_events_per_task: usize,
}

impl SqliteEventStore {
    /// Opens or creates a database file.
    ///
    /// New files are created with mode `0600` on Unix. Existing Unix files
    /// must already deny group and world permissions. Symbolic links are
    /// rejected.
    pub fn open(
        path: impl AsRef<Path>,
        max_events_per_task: usize,
    ) -> Result<Self, EventStoreError> {
        if max_events_per_task == 0 {
            return Err(EventStoreError::CapacityExceeded);
        }

        let path = path.as_ref();
        prepare_database_file(path)?;
        let connection = Connection::open(path).map_err(|_| EventStoreError::Unavailable)?;
        Self::initialize(connection, max_events_per_task, true)
    }

    /// Creates an isolated in-memory database for tests and ephemeral use.
    pub fn open_in_memory(max_events_per_task: usize) -> Result<Self, EventStoreError> {
        if max_events_per_task == 0 {
            return Err(EventStoreError::CapacityExceeded);
        }

        let connection = Connection::open_in_memory().map_err(|_| EventStoreError::Unavailable)?;
        Self::initialize(connection, max_events_per_task, false)
    }

    /// Reconstructs the latest public state of every task from its events.
    ///
    /// This does not restore Task input or resume execution.
    pub fn recover_task_snapshots(&self) -> Result<Vec<TaskSnapshot>, EventStoreError> {
        let task_ids = {
            let mut statement = self
                .connection
                .prepare("SELECT DISTINCT task_id FROM task_events ORDER BY task_id")
                .map_err(|_| EventStoreError::Unavailable)?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|_| EventStoreError::Unavailable)?;

            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|_| EventStoreError::Unavailable)?
        };

        task_ids
            .into_iter()
            .map(|task_id| {
                let task_id = task_id
                    .parse::<TaskId>()
                    .map_err(|_| EventStoreError::Corrupt)?;
                let events = self.list(task_id, 0)?;
                recover_snapshot(task_id, &events)
            })
            .collect()
    }

    fn initialize(
        connection: Connection,
        max_events_per_task: usize,
        enable_wal: bool,
    ) -> Result<Self, EventStoreError> {
        connection
            .busy_timeout(DEFAULT_BUSY_TIMEOUT)
            .map_err(|_| EventStoreError::Unavailable)?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|_| EventStoreError::Unavailable)?;
        if enable_wal {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|_| EventStoreError::Unavailable)?;
        }

        let schema_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| EventStoreError::Unavailable)?;
        match schema_version {
            0 => {
                let transaction = connection
                    .unchecked_transaction()
                    .map_err(|_| EventStoreError::Unavailable)?;
                transaction
                    .execute_batch(CREATE_SCHEMA)
                    .map_err(|_| EventStoreError::Unavailable)?;
                transaction
                    .pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(|_| EventStoreError::Unavailable)?;
                transaction
                    .commit()
                    .map_err(|_| EventStoreError::Unavailable)?;
            }
            SCHEMA_VERSION => {}
            _ => return Err(EventStoreError::Corrupt),
        }

        Ok(Self {
            connection,
            max_events_per_task,
        })
    }
}

impl EventStore for SqliteEventStore {
    fn append_batch(
        &mut self,
        task_id: TaskId,
        kinds: &[TaskEventKind],
    ) -> Result<Vec<TaskEvent>, EventStoreError> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| EventStoreError::Unavailable)?;
        let task_id_text = task_id.to_string();
        let (existing_count, last_sequence): (i64, i64) = transaction
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(sequence), 0)
                 FROM task_events WHERE task_id = ?1",
                params![task_id_text],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| EventStoreError::Unavailable)?;
        let existing_count =
            usize::try_from(existing_count).map_err(|_| EventStoreError::Corrupt)?;
        let resulting_count = existing_count
            .checked_add(kinds.len())
            .ok_or(EventStoreError::CapacityExceeded)?;
        if resulting_count > self.max_events_per_task {
            return Err(EventStoreError::CapacityExceeded);
        }

        let first_sequence = u64::try_from(last_sequence)
            .map_err(|_| EventStoreError::Corrupt)?
            .checked_add(1)
            .ok_or(EventStoreError::SequenceExhausted)?;
        let mut appended = Vec::with_capacity(kinds.len());
        for (offset, kind) in kinds.iter().enumerate() {
            let offset = u64::try_from(offset).map_err(|_| EventStoreError::SequenceExhausted)?;
            let sequence = first_sequence
                .checked_add(offset)
                .ok_or(EventStoreError::SequenceExhausted)?;
            let event = TaskEvent::now(task_id, sequence, kind.clone())?;
            let sequence_sql =
                i64::try_from(event.sequence).map_err(|_| EventStoreError::SequenceExhausted)?;
            let occurred_at_sql = i64::try_from(event.occurred_at_unix_ms)
                .map_err(|_| EventStoreError::SequenceExhausted)?;
            let event_json =
                serde_json::to_string(&event.kind).map_err(|_| EventStoreError::Unavailable)?;

            transaction
                .execute(
                    "INSERT INTO task_events
                     (task_id, sequence, occurred_at_unix_ms, event_json)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![task_id_text, sequence_sql, occurred_at_sql, event_json],
                )
                .map_err(|_| EventStoreError::Unavailable)?;
            appended.push(event);
        }

        transaction
            .commit()
            .map_err(|_| EventStoreError::Unavailable)?;
        Ok(appended)
    }

    fn list(
        &self,
        task_id: TaskId,
        after_sequence: u64,
    ) -> Result<Vec<TaskEvent>, EventStoreError> {
        let after_sequence =
            i64::try_from(after_sequence).map_err(|_| EventStoreError::SequenceExhausted)?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT sequence, occurred_at_unix_ms, event_json
                 FROM task_events
                 WHERE task_id = ?1 AND sequence > ?2
                 ORDER BY sequence",
            )
            .map_err(|_| EventStoreError::Unavailable)?;
        let rows = statement
            .query_map(params![task_id.to_string(), after_sequence], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| EventStoreError::Unavailable)?;

        rows.map(|row| {
            let (sequence, occurred_at_unix_ms, event_json) =
                row.map_err(|_| EventStoreError::Unavailable)?;
            let sequence = u64::try_from(sequence).map_err(|_| EventStoreError::Corrupt)?;
            let occurred_at_unix_ms =
                u64::try_from(occurred_at_unix_ms).map_err(|_| EventStoreError::Corrupt)?;
            let kind = serde_json::from_str(&event_json).map_err(|_| EventStoreError::Corrupt)?;
            Ok(TaskEvent {
                task_id,
                sequence,
                occurred_at_unix_ms,
                kind,
            })
        })
        .collect()
    }
}

impl RecoverableEventStore for SqliteEventStore {
    fn recover_task_snapshots(&self) -> Result<Vec<TaskSnapshot>, EventStoreError> {
        Self::recover_task_snapshots(self)
    }
}

fn recover_snapshot(
    task_id: TaskId,
    events: &[TaskEvent],
) -> Result<TaskSnapshot, EventStoreError> {
    let mut state = None;
    let mut expected_sequence = 1_u64;
    let mut pending_failure = false;

    for event in events {
        if event.sequence != expected_sequence {
            return Err(EventStoreError::Corrupt);
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(EventStoreError::SequenceExhausted)?;

        if pending_failure {
            match &event.kind {
                TaskEventKind::StateTransitioned { from, to }
                    if state == Some(*from) && *to == TaskState::Failed =>
                {
                    state = Some(*to);
                    pending_failure = false;
                    continue;
                }
                _ => return Err(EventStoreError::Corrupt),
            }
        }

        match event.kind.clone() {
            TaskEventKind::Submitted if state.is_none() => state = Some(TaskState::Submitted),
            TaskEventKind::StateTransitioned { from, to }
                if state == Some(from) && from.can_transition_to(to) =>
            {
                state = Some(to);
            }
            TaskEventKind::ValidationFailed { .. } if state == Some(TaskState::Validating) => {}
            TaskEventKind::TaskFailed { .. }
                if state.is_some_and(|current| !current.is_terminal()) =>
            {
                pending_failure = true;
            }
            TaskEventKind::OperationAllowed | TaskEventKind::OperationDenied { .. }
                if state == Some(TaskState::Running) => {}
            TaskEventKind::ApprovalRequested { .. } if state == Some(TaskState::Running) => {}
            TaskEventKind::ApprovalGranted { .. }
            | TaskEventKind::ApprovalDenied { .. }
            | TaskEventKind::ApprovalExpired { .. }
                if state == Some(TaskState::WaitingApproval) => {}
            TaskEventKind::ApprovalRevoked { .. }
                if matches!(state, Some(TaskState::Running | TaskState::WaitingApproval)) => {}
            TaskEventKind::ApprovalConsumed { .. } if state == Some(TaskState::Running) => {}
            TaskEventKind::Submitted
            | TaskEventKind::StateTransitioned { .. }
            | TaskEventKind::ValidationFailed { .. }
            | TaskEventKind::TaskFailed { .. }
            | TaskEventKind::OperationAllowed
            | TaskEventKind::OperationDenied { .. }
            | TaskEventKind::ApprovalRequested { .. }
            | TaskEventKind::ApprovalGranted { .. }
            | TaskEventKind::ApprovalDenied { .. }
            | TaskEventKind::ApprovalExpired { .. }
            | TaskEventKind::ApprovalRevoked { .. }
            | TaskEventKind::ApprovalConsumed { .. } => return Err(EventStoreError::Corrupt),
        }
    }

    if pending_failure {
        return Err(EventStoreError::Corrupt);
    }

    state
        .map(|state| TaskSnapshot { task_id, state })
        .ok_or(EventStoreError::Corrupt)
}

fn prepare_database_file(path: &Path) -> Result<(), EventStoreError> {
    match path.symlink_metadata() {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(EventStoreError::InsecurePermissions);
            }
            #[cfg(unix)]
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(EventStoreError::InsecurePermissions);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options
                .open(path)
                .map_err(|_| EventStoreError::Unavailable)?;
        }
        Err(_) => return Err(EventStoreError::Unavailable),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use aios_core::{ErrorCode, TaskSpec, TaskState};
    use aios_runtime::{
        ApprovalAuthority, EventStore, EventStoreError, OperationId, SubmitResult, TaskEventKind,
        TaskId, TaskSupervisor,
    };
    use rusqlite::params;

    use super::SqliteEventStore;

    struct TestDatabase {
        path: PathBuf,
    }

    impl TestDatabase {
        fn new() -> Self {
            Self {
                path: std::env::temp_dir().join(format!("aios-{}.sqlite", TaskId::new())),
            }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            for path in [
                self.path.clone(),
                self.path.with_extension("sqlite-wal"),
                self.path.with_extension("sqlite-shm"),
            ] {
                let _ = fs::remove_file(path);
            }
        }
    }

    fn queued_events() -> [TaskEventKind; 3] {
        [
            TaskEventKind::Submitted,
            TaskEventKind::StateTransitioned {
                from: TaskState::Submitted,
                to: TaskState::Validating,
            },
            TaskEventKind::StateTransitioned {
                from: TaskState::Validating,
                to: TaskState::Queued,
            },
        ]
    }

    #[test]
    fn persists_events_across_reopen() {
        let database = TestDatabase::new();
        let task_id = TaskId::new();

        {
            let mut store = SqliteEventStore::open(database.path(), 100).expect("open database");
            store
                .append_batch(task_id, &queued_events())
                .expect("append events");
        }

        let store = SqliteEventStore::open(database.path(), 100).expect("reopen database");
        let events = store.list(task_id, 0).expect("list events");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[2].sequence, 3);
    }

    #[test]
    fn recovers_latest_task_state_from_events() {
        let mut store = SqliteEventStore::open_in_memory(100).expect("open database");
        let queued_task = TaskId::new();
        let running_task = TaskId::new();
        store
            .append_batch(queued_task, &queued_events())
            .expect("append queued task");
        store
            .append_batch(running_task, &queued_events())
            .expect("append running task");
        store
            .append_batch(
                running_task,
                &[TaskEventKind::StateTransitioned {
                    from: TaskState::Queued,
                    to: TaskState::Running,
                }],
            )
            .expect("append running state");

        let snapshots = store.recover_task_snapshots().expect("recover tasks");
        assert_eq!(snapshots.len(), 2);
        assert!(
            snapshots
                .iter()
                .any(|task| { task.task_id == queued_task && task.state == TaskState::Queued })
        );
        assert!(
            snapshots
                .iter()
                .any(|task| { task.task_id == running_task && task.state == TaskState::Running })
        );
    }

    #[test]
    fn recovers_state_across_resource_free_approval_events() {
        let mut store = SqliteEventStore::open_in_memory(100).expect("open database");
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let mut authority = ApprovalAuthority::default();
        let approval = authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("request approval");
        let mut events = queued_events().to_vec();
        events.extend([
            TaskEventKind::StateTransitioned {
                from: TaskState::Queued,
                to: TaskState::Running,
            },
            TaskEventKind::ApprovalRequested {
                approval_id: approval.approval_id,
                operation_id,
            },
            TaskEventKind::StateTransitioned {
                from: TaskState::Running,
                to: TaskState::WaitingApproval,
            },
            TaskEventKind::ApprovalGranted {
                approval_id: approval.approval_id,
                operation_id,
            },
            TaskEventKind::StateTransitioned {
                from: TaskState::WaitingApproval,
                to: TaskState::Running,
            },
            TaskEventKind::ApprovalConsumed {
                approval_id: approval.approval_id,
                operation_id,
            },
            TaskEventKind::StateTransitioned {
                from: TaskState::Running,
                to: TaskState::Succeeded,
            },
        ]);
        store
            .append_batch(task_id, &events)
            .expect("append approval lifecycle");

        let snapshots = store.recover_task_snapshots().expect("recover tasks");

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].task_id, task_id);
        assert_eq!(snapshots[0].state, TaskState::Succeeded);
    }

    #[test]
    fn restart_fails_non_terminal_tasks_once_and_preserves_terminal_tasks() {
        let database = TestDatabase::new();
        let queued_task = TaskId::new();
        let running_task = TaskId::new();
        let waiting_task = TaskId::new();
        let succeeded_task = TaskId::new();

        {
            let mut store = SqliteEventStore::open(database.path(), 100).expect("open database");
            for task_id in [queued_task, running_task, waiting_task, succeeded_task] {
                store
                    .append_batch(task_id, &queued_events())
                    .expect("append queued task");
            }
            for task_id in [running_task, waiting_task, succeeded_task] {
                store
                    .append_batch(
                        task_id,
                        &[TaskEventKind::StateTransitioned {
                            from: TaskState::Queued,
                            to: TaskState::Running,
                        }],
                    )
                    .expect("append running state");
            }
            let operation_id = OperationId::new();
            let mut authority = ApprovalAuthority::default();
            let approval = authority
                .request(
                    waiting_task,
                    operation_id,
                    "git.commit",
                    Duration::from_secs(30),
                )
                .expect("request approval");
            store
                .append_batch(
                    waiting_task,
                    &[
                        TaskEventKind::ApprovalRequested {
                            approval_id: approval.approval_id,
                            operation_id,
                        },
                        TaskEventKind::StateTransitioned {
                            from: TaskState::Running,
                            to: TaskState::WaitingApproval,
                        },
                    ],
                )
                .expect("append waiting state");
            store
                .append_batch(
                    succeeded_task,
                    &[TaskEventKind::StateTransitioned {
                        from: TaskState::Running,
                        to: TaskState::Succeeded,
                    }],
                )
                .expect("append success state");
        }

        {
            let store = SqliteEventStore::open(database.path(), 100).expect("reopen database");
            let mut supervisor = TaskSupervisor::recover(store, 10).expect("recover supervisor");

            for task_id in [queued_task, running_task, waiting_task] {
                assert_eq!(
                    supervisor.get(task_id).expect("recovered task").state,
                    TaskState::Failed
                );
                let events = supervisor.events(task_id, 0).expect("recovered events");
                assert!(matches!(
                    events[events.len() - 2].kind,
                    TaskEventKind::TaskFailed {
                        code: ErrorCode::RuntimeRestarted
                    }
                ));
                assert_eq!(
                    events.last().expect("terminal transition").kind,
                    TaskEventKind::StateTransitioned {
                        from: match task_id {
                            id if id == queued_task => TaskState::Queued,
                            id if id == running_task => TaskState::Running,
                            _ => TaskState::WaitingApproval,
                        },
                        to: TaskState::Failed,
                    }
                );
            }

            assert_eq!(
                supervisor.get(succeeded_task).expect("terminal task").state,
                TaskState::Succeeded
            );
            assert_eq!(
                supervisor
                    .events(succeeded_task, 0)
                    .expect("terminal events")
                    .len(),
                5
            );

            let task: TaskSpec = serde_json::from_str(include_str!("../../../examples/task.json"))
                .expect("valid example task");
            let SubmitResult::Accepted(resubmitted) =
                supervisor.submit(task).expect("explicit resubmission")
            else {
                panic!("resubmission must create a new Task");
            };
            assert!(
                ![queued_task, running_task, waiting_task, succeeded_task]
                    .contains(&resubmitted.task_id)
            );
        }

        let store = SqliteEventStore::open(database.path(), 100).expect("reopen database again");
        let supervisor = TaskSupervisor::recover(store, 10).expect("repeat recovery");
        assert_eq!(
            supervisor
                .events(queued_task, 0)
                .expect("queued events")
                .len(),
            5
        );
        assert_eq!(
            supervisor
                .events(running_task, 0)
                .expect("running events")
                .len(),
            6
        );
        assert_eq!(
            supervisor
                .events(waiting_task, 0)
                .expect("waiting events")
                .len(),
            8
        );
    }

    #[test]
    fn restart_audit_failure_prevents_supervisor_recovery() {
        let database = TestDatabase::new();
        let task_id = TaskId::new();
        {
            let mut store = SqliteEventStore::open(database.path(), 3).expect("open database");
            store
                .append_batch(task_id, &queued_events())
                .expect("append queued task");
        }

        let store = SqliteEventStore::open(database.path(), 3).expect("reopen database");
        assert!(matches!(
            TaskSupervisor::recover(store, 10),
            Err(aios_runtime::SupervisorError::EventStore(
                EventStoreError::CapacityExceeded
            ))
        ));

        let store = SqliteEventStore::open(database.path(), 100).expect("inspect database");
        let snapshots = store.recover_task_snapshots().expect("recover snapshots");
        assert_eq!(snapshots[0].state, TaskState::Queued);
        assert_eq!(store.list(task_id, 0).expect("events").len(), 3);
    }

    #[test]
    fn rejects_unpaired_failure_category_during_recovery() {
        let mut store = SqliteEventStore::open_in_memory(100).expect("open database");
        let task_id = TaskId::new();
        store
            .append_batch(task_id, &queued_events())
            .expect("append queued task");
        store
            .append_batch(
                task_id,
                &[TaskEventKind::TaskFailed {
                    code: ErrorCode::RuntimeRestarted,
                }],
            )
            .expect("append incomplete failure");

        assert_eq!(
            store.recover_task_snapshots(),
            Err(EventStoreError::Corrupt)
        );
    }

    #[test]
    fn capacity_failure_rolls_back_the_whole_batch() {
        let mut store = SqliteEventStore::open_in_memory(2).expect("open database");
        let task_id = TaskId::new();

        let error = store
            .append_batch(task_id, &queued_events())
            .expect_err("batch exceeds capacity");

        assert_eq!(error, EventStoreError::CapacityExceeded);
        assert!(store.list(task_id, 0).expect("list events").is_empty());
    }

    #[test]
    fn detects_corrupt_event_payload() {
        let store = SqliteEventStore::open_in_memory(100).expect("open database");
        let task_id = TaskId::new();
        store
            .connection
            .execute(
                "INSERT INTO task_events
                 (task_id, sequence, occurred_at_unix_ms, event_json)
                 VALUES (?1, 1, 0, ?2)",
                params![task_id.to_string(), "not-json"],
            )
            .expect("insert corrupt event");

        assert_eq!(store.list(task_id, 0), Err(EventStoreError::Corrupt));
    }

    #[test]
    fn rejects_recovery_when_event_sequence_has_a_gap() {
        let store = SqliteEventStore::open_in_memory(100).expect("open database");
        let task_id = TaskId::new();
        let event_json = serde_json::to_string(&TaskEventKind::Submitted).expect("serialize event");
        store
            .connection
            .execute(
                "INSERT INTO task_events
                 (task_id, sequence, occurred_at_unix_ms, event_json)
                 VALUES (?1, 2, 0, ?2)",
                params![task_id.to_string(), event_json],
            )
            .expect("insert event with gap");

        assert_eq!(
            store.recover_task_snapshots(),
            Err(EventStoreError::Corrupt)
        );
    }

    #[cfg(unix)]
    #[test]
    fn creates_database_with_owner_only_permissions() {
        let database = TestDatabase::new();
        let _store = SqliteEventStore::open(database.path(), 100).expect("open database");

        let mode = fs::metadata(database.path())
            .expect("database metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_existing_database_with_public_permissions() {
        let database = TestDatabase::new();
        fs::write(database.path(), []).expect("create database file");
        fs::set_permissions(database.path(), fs::Permissions::from_mode(0o644))
            .expect("set permissions");

        assert!(matches!(
            SqliteEventStore::open(database.path(), 100),
            Err(EventStoreError::InsecurePermissions)
        ));
    }

    #[test]
    fn returns_only_events_after_the_requested_sequence() {
        let mut store = SqliteEventStore::open_in_memory(100).expect("open database");
        let task_id = TaskId::new();
        store
            .append_batch(task_id, &queued_events())
            .expect("append events");

        let events = store.list(task_id, 2).expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 3);
    }
}
