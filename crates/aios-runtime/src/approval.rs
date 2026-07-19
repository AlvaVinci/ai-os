use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
use std::time::{Duration, Instant};

use serde::Serialize;
use uuid::Uuid;

use crate::TaskId;

const DEFAULT_MAX_PENDING_APPROVALS: usize = 1_024;
const DEFAULT_MAX_TTL: Duration = Duration::from_secs(15 * 60);
const MAX_MAX_PENDING_APPROVALS: usize = 65_536;
const MAX_MAX_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ACTION_BYTES: usize = 64;

/// Public identifier for one pending human approval.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ApprovalId(Uuid);

impl Display for ApprovalId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl FromStr for ApprovalId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Opaque identifier that a trusted adapter binds to one exact operation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OperationId(Uuid);

impl OperationId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for OperationId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl FromStr for OperationId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Non-sensitive data a user interface may present for one approval request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalRequest {
    pub approval_id: ApprovalId,
    pub task_id: TaskId,
    pub operation_id: OperationId,
    pub action: String,
    pub expires_after_ms: u64,
}

/// Proof that one linear grant was consumed successfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ApprovalReceipt {
    pub approval_id: ApprovalId,
}

/// A one-time grant. It intentionally cannot be cloned, debugged, or serialized.
#[must_use = "an approval grant must be consumed or explicitly dropped"]
pub struct ApprovalGrant {
    approval_id: ApprovalId,
    task_id: TaskId,
    operation_id: OperationId,
    action: String,
    deadline: Instant,
}

impl ApprovalGrant {
    /// Consumes the grant while verifying its exact task, operation, action, and deadline.
    pub fn authorize(
        self,
        task_id: TaskId,
        operation_id: OperationId,
        action: &str,
    ) -> Result<ApprovalReceipt, ApprovalError> {
        if Instant::now() >= self.deadline {
            return Err(ApprovalError::Expired);
        }
        if self.task_id != task_id || self.operation_id != operation_id || self.action != action {
            return Err(ApprovalError::ScopeMismatch);
        }
        Ok(ApprovalReceipt {
            approval_id: self.approval_id,
        })
    }
}

struct PendingApproval {
    task_id: TaskId,
    operation_id: OperationId,
    action: String,
    deadline: Instant,
}

/// Bounded in-memory authority for pending approval requests and linear grants.
pub struct ApprovalAuthority {
    pending: BTreeMap<ApprovalId, PendingApproval>,
    operation_index: BTreeMap<(TaskId, OperationId), ApprovalId>,
    max_pending: usize,
    max_ttl: Duration,
}

impl ApprovalAuthority {
    pub fn new(max_pending: usize, max_ttl: Duration) -> Result<Self, ApprovalError> {
        if max_pending == 0
            || max_pending > MAX_MAX_PENDING_APPROVALS
            || max_ttl.is_zero()
            || max_ttl > MAX_MAX_TTL
        {
            return Err(ApprovalError::InvalidConfig);
        }
        Ok(Self {
            pending: BTreeMap::new(),
            operation_index: BTreeMap::new(),
            max_pending,
            max_ttl,
        })
    }

    /// Registers one exact operation for later human approval.
    pub fn request(
        &mut self,
        task_id: TaskId,
        operation_id: OperationId,
        action: &str,
        ttl: Duration,
    ) -> Result<ApprovalRequest, ApprovalError> {
        self.purge_expired();
        if !is_valid_action(action) || ttl < Duration::from_millis(1) || ttl > self.max_ttl {
            return Err(ApprovalError::InvalidRequest);
        }
        if self.operation_index.contains_key(&(task_id, operation_id)) {
            return Err(ApprovalError::DuplicateOperation);
        }
        if self.pending.len() >= self.max_pending {
            return Err(ApprovalError::CapacityExceeded);
        }

        let deadline = Instant::now()
            .checked_add(ttl)
            .ok_or(ApprovalError::InvalidRequest)?;
        let approval_id = ApprovalId(Uuid::now_v7());
        let action = action.to_owned();
        self.pending.insert(
            approval_id,
            PendingApproval {
                task_id,
                operation_id,
                action: action.clone(),
                deadline,
            },
        );
        self.operation_index
            .insert((task_id, operation_id), approval_id);

        Ok(ApprovalRequest {
            approval_id,
            task_id,
            operation_id,
            action,
            expires_after_ms: duration_ms(ttl)?,
        })
    }

    /// Approves and removes one pending request, returning a linear grant.
    pub fn approve(&mut self, approval_id: ApprovalId) -> Result<ApprovalGrant, ApprovalError> {
        let pending = self.remove(approval_id).ok_or(ApprovalError::NotFound)?;
        if Instant::now() >= pending.deadline {
            return Err(ApprovalError::Expired);
        }
        Ok(ApprovalGrant {
            approval_id,
            task_id: pending.task_id,
            operation_id: pending.operation_id,
            action: pending.action,
            deadline: pending.deadline,
        })
    }

    /// Denies and removes one pending request.
    pub fn deny(&mut self, approval_id: ApprovalId) -> Result<(), ApprovalError> {
        let pending = self.remove(approval_id).ok_or(ApprovalError::NotFound)?;
        if Instant::now() >= pending.deadline {
            return Err(ApprovalError::Expired);
        }
        Ok(())
    }

    pub fn pending_count(&mut self) -> usize {
        self.purge_expired();
        self.pending.len()
    }

    fn purge_expired(&mut self) {
        let now = Instant::now();
        let expired: Vec<ApprovalId> = self
            .pending
            .iter()
            .filter_map(|(approval_id, pending)| (now >= pending.deadline).then_some(*approval_id))
            .collect();
        for approval_id in expired {
            let _ = self.remove(approval_id);
        }
    }

    fn remove(&mut self, approval_id: ApprovalId) -> Option<PendingApproval> {
        let pending = self.pending.remove(&approval_id)?;
        self.operation_index
            .remove(&(pending.task_id, pending.operation_id));
        Some(pending)
    }
}

impl Default for ApprovalAuthority {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_PENDING_APPROVALS, DEFAULT_MAX_TTL)
            .expect("default approval configuration must be valid")
    }
}

/// Approval lifecycle failure without resource or token values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalError {
    InvalidConfig,
    InvalidRequest,
    DuplicateOperation,
    CapacityExceeded,
    NotFound,
    Expired,
    ScopeMismatch,
}

impl Display for ApprovalError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfig => "invalid approval authority configuration",
            Self::InvalidRequest => "approval request is invalid",
            Self::DuplicateOperation => "operation already has a pending approval",
            Self::CapacityExceeded => "pending approval capacity exceeded",
            Self::NotFound => "approval request not found",
            Self::Expired => "approval expired",
            Self::ScopeMismatch => "approval scope does not match",
        };
        formatter.write_str(message)
    }
}

impl Error for ApprovalError {}

fn is_valid_action(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= MAX_ACTION_BYTES
        && action.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}

fn duration_ms(duration: Duration) -> Result<u64, ApprovalError> {
    u64::try_from(duration.as_millis()).map_err(|_| ApprovalError::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{ApprovalAuthority, ApprovalError, OperationId};
    use crate::TaskId;

    fn authority(max_pending: usize) -> ApprovalAuthority {
        ApprovalAuthority::new(max_pending, Duration::from_secs(60)).expect("valid authority")
    }

    #[test]
    fn rejects_unsafe_configuration() {
        assert!(matches!(
            ApprovalAuthority::new(0, Duration::from_secs(1)),
            Err(ApprovalError::InvalidConfig)
        ));
        assert!(matches!(
            ApprovalAuthority::new(1, Duration::ZERO),
            Err(ApprovalError::InvalidConfig)
        ));
        assert!(matches!(
            ApprovalAuthority::new(1, Duration::from_secs(86_401)),
            Err(ApprovalError::InvalidConfig)
        ));
    }

    #[test]
    fn request_is_task_operation_action_and_expiry_scoped() {
        let mut authority = authority(1);
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let request = authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("request approval");

        assert_eq!(request.task_id, task_id);
        assert_eq!(request.operation_id, operation_id);
        assert_eq!(request.action, "git.commit");
        assert_eq!(request.expires_after_ms, 30_000);
        assert_eq!(authority.pending_count(), 1);
    }

    #[test]
    fn rejects_invalid_action_and_ttl_without_echoing_values() {
        let mut authority = authority(1);
        let error = authority
            .request(
                TaskId::new(),
                OperationId::new(),
                "git.commit;secret-value",
                Duration::from_secs(1),
            )
            .expect_err("invalid action");

        assert_eq!(error, ApprovalError::InvalidRequest);
        assert!(!error.to_string().contains("secret-value"));
        assert!(matches!(
            authority.request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(61),
            ),
            Err(ApprovalError::InvalidRequest)
        ));
        assert!(matches!(
            authority.request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_nanos(1),
            ),
            Err(ApprovalError::InvalidRequest)
        ));
    }

    #[test]
    fn duplicate_operation_and_capacity_are_bounded() {
        let mut authority = authority(1);
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("first request");

        assert!(matches!(
            authority.request(task_id, operation_id, "git.commit", Duration::from_secs(30)),
            Err(ApprovalError::DuplicateOperation)
        ));
        assert!(matches!(
            authority.request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(30),
            ),
            Err(ApprovalError::CapacityExceeded)
        ));
    }

    #[test]
    fn denial_removes_pending_request_and_frees_capacity() {
        let mut authority = authority(1);
        let first = authority
            .request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(30),
            )
            .expect("first request");
        authority.deny(first.approval_id).expect("deny request");

        assert_eq!(authority.pending_count(), 0);
        assert!(matches!(
            authority.deny(first.approval_id),
            Err(ApprovalError::NotFound)
        ));
        authority
            .request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(30),
            )
            .expect("capacity was freed");
    }

    #[test]
    fn approved_grant_authorizes_exact_scope_once() {
        let mut authority = authority(1);
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let request = authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("request approval");
        let grant = authority
            .approve(request.approval_id)
            .expect("approve request");

        let receipt = grant
            .authorize(task_id, operation_id, "git.commit")
            .expect("authorize operation");
        assert_eq!(receipt.approval_id, request.approval_id);
        assert_eq!(authority.pending_count(), 0);
        assert!(matches!(
            authority.approve(request.approval_id),
            Err(ApprovalError::NotFound)
        ));
    }

    #[test]
    fn scope_mismatch_consumes_linear_grant() {
        let mut authority = authority(1);
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let request = authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("request approval");
        let grant = authority
            .approve(request.approval_id)
            .expect("approve request");

        assert_eq!(
            grant.authorize(TaskId::new(), operation_id, "git.commit"),
            Err(ApprovalError::ScopeMismatch)
        );
    }

    #[test]
    fn expired_request_cannot_be_approved() {
        let mut authority = authority(1);
        let request = authority
            .request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(30),
            )
            .expect("request approval");
        authority
            .pending
            .get_mut(&request.approval_id)
            .expect("pending request")
            .deadline = Instant::now();

        assert!(matches!(
            authority.approve(request.approval_id),
            Err(ApprovalError::Expired)
        ));
        assert_eq!(authority.pending_count(), 0);
    }

    #[test]
    fn pending_count_purges_expired_requests() {
        let mut authority = authority(1);
        let request = authority
            .request(
                TaskId::new(),
                OperationId::new(),
                "git.commit",
                Duration::from_secs(30),
            )
            .expect("request approval");
        authority
            .pending
            .get_mut(&request.approval_id)
            .expect("pending request")
            .deadline = Instant::now();

        assert_eq!(authority.pending_count(), 0);
        assert!(matches!(
            authority.approve(request.approval_id),
            Err(ApprovalError::NotFound)
        ));
    }

    #[test]
    fn expired_grant_cannot_authorize() {
        let mut authority = authority(1);
        let task_id = TaskId::new();
        let operation_id = OperationId::new();
        let request = authority
            .request(task_id, operation_id, "git.commit", Duration::from_secs(30))
            .expect("request approval");
        let mut grant = authority
            .approve(request.approval_id)
            .expect("approve request");
        grant.deadline = Instant::now();

        assert_eq!(
            grant.authorize(task_id, operation_id, "git.commit"),
            Err(ApprovalError::Expired)
        );
    }
}
