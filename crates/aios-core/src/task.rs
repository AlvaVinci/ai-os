use std::collections::BTreeSet;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::{ValidationError, ValidationErrors};

const MAX_GOAL_CHARS: usize = 8_192;
const MAX_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAX_FILESYSTEM_CAPABILITIES: usize = 128;
const MAX_TOOLS: usize = 64;
const MAX_APPROVAL_ACTIONS: usize = 64;
const MAX_NETWORK_DESTINATIONS: usize = 64;
const MAX_PATH_BYTES: usize = 4_096;
const MAX_IDENTIFIER_BYTES: usize = 64;

/// A user request and the constraints under which it may execute.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub idempotency_key: String,
    pub goal: String,
    pub capabilities: CapabilitySet,
    pub budget: Budget,
    pub approval: ApprovalPolicy,
}

impl TaskSpec {
    /// Validates all fields without returning sensitive input values in errors.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = Vec::new();

        validate_goal(&self.goal, &mut errors);
        validate_identifier(
            "idempotency_key",
            &self.idempotency_key,
            MAX_IDEMPOTENCY_KEY_BYTES,
            &mut errors,
        );
        self.capabilities.validate(&mut errors);
        self.budget.validate(&mut errors);
        self.approval.validate(&mut errors);

        match ValidationErrors::new(errors) {
            Some(errors) => Err(errors),
            None => Ok(()),
        }
    }
}

/// Explicit resources that an agent may request.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySet {
    #[serde(default)]
    pub filesystem: Vec<FileCapability>,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub tools: Vec<String>,
}

impl CapabilitySet {
    fn validate(&self, errors: &mut Vec<ValidationError>) {
        if self.filesystem.len() > MAX_FILESYSTEM_CAPABILITIES {
            errors.push(ValidationError::new(
                "capabilities.filesystem",
                format!("must contain at most {MAX_FILESYSTEM_CAPABILITIES} entries"),
            ));
        }

        let mut seen = BTreeSet::new();
        for capability in &self.filesystem {
            validate_absolute_normalized_path(&capability.path, errors);
            if !seen.insert((&capability.path, capability.access)) {
                errors.push(ValidationError::new(
                    "capabilities.filesystem",
                    "must not contain duplicate path and access entries",
                ));
            }
        }

        self.network.validate(errors);

        if self.tools.len() > MAX_TOOLS {
            errors.push(ValidationError::new(
                "capabilities.tools",
                format!("must contain at most {MAX_TOOLS} entries"),
            ));
        }
        let mut seen_tools = BTreeSet::new();
        for tool in &self.tools {
            validate_identifier("capabilities.tools", tool, MAX_IDENTIFIER_BYTES, errors);
            if !seen_tools.insert(tool) {
                errors.push(ValidationError::new(
                    "capabilities.tools",
                    "must not contain duplicate entries",
                ));
            }
        }
    }
}

/// Filesystem access to one normalized absolute path.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileCapability {
    pub path: String,
    pub access: FileAccess,
}

/// Operations allowed for a filesystem capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    Read,
    Write,
}

/// Outbound network policy. Deny is the safe default.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPolicy {
    #[default]
    Deny,
    Allow {
        destinations: Vec<NetworkDestination>,
    },
}

/// One exact outbound network destination.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkDestination {
    pub host: String,
    pub transport: NetworkTransport,
    pub port: u16,
}

/// Transport supported by the current Network Capability contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkTransport {
    Tcp,
}

impl NetworkPolicy {
    fn validate(&self, errors: &mut Vec<ValidationError>) {
        let Self::Allow { destinations } = self else {
            return;
        };

        if destinations.is_empty() || destinations.len() > MAX_NETWORK_DESTINATIONS {
            errors.push(ValidationError::new(
                "capabilities.network.destinations",
                format!("must contain between 1 and {MAX_NETWORK_DESTINATIONS} entries"),
            ));
        }

        let mut seen_destinations = BTreeSet::new();
        for destination in destinations {
            if !is_valid_network_host(&destination.host) {
                errors.push(ValidationError::new(
                    "capabilities.network.destinations.host",
                    "must contain lowercase host names or IP addresses without schemes or paths",
                ));
            }
            if destination.port == 0 {
                errors.push(ValidationError::new(
                    "capabilities.network.destinations.port",
                    "must be between 1 and 65535",
                ));
            }
            if !seen_destinations.insert((
                &destination.host,
                destination.transport,
                destination.port,
            )) {
                errors.push(ValidationError::new(
                    "capabilities.network.destinations",
                    "must not contain duplicate entries",
                ));
            }
        }
    }
}

/// Resource limits enforced for one task.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub wall_time_seconds: u64,
    pub memory_bytes: u64,
    pub max_parallel_agents: u8,
}

impl Budget {
    fn validate(&self, errors: &mut Vec<ValidationError>) {
        if self.wall_time_seconds == 0 {
            errors.push(ValidationError::new(
                "budget.wall_time_seconds",
                "must be greater than zero",
            ));
        }
        if self.memory_bytes == 0 {
            errors.push(ValidationError::new(
                "budget.memory_bytes",
                "must be greater than zero",
            ));
        }
        if !(1..=8).contains(&self.max_parallel_agents) {
            errors.push(ValidationError::new(
                "budget.max_parallel_agents",
                "must be between 1 and 8",
            ));
        }
    }
}

/// Operations that require a fresh, task-scoped user approval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalPolicy {
    #[serde(default)]
    pub required_for: Vec<String>,
}

impl ApprovalPolicy {
    fn validate(&self, errors: &mut Vec<ValidationError>) {
        if self.required_for.len() > MAX_APPROVAL_ACTIONS {
            errors.push(ValidationError::new(
                "approval.required_for",
                format!("must contain at most {MAX_APPROVAL_ACTIONS} entries"),
            ));
        }
        let mut seen_actions = BTreeSet::new();
        for action in &self.required_for {
            validate_identifier(
                "approval.required_for",
                action,
                MAX_IDENTIFIER_BYTES,
                errors,
            );
            if !seen_actions.insert(action) {
                errors.push(ValidationError::new(
                    "approval.required_for",
                    "must not contain duplicate entries",
                ));
            }
        }
    }
}

fn validate_goal(goal: &str, errors: &mut Vec<ValidationError>) {
    let length = goal.chars().count();
    if goal.trim().is_empty() || length > MAX_GOAL_CHARS {
        errors.push(ValidationError::new(
            "goal",
            format!("must contain between 1 and {MAX_GOAL_CHARS} non-blank characters"),
        ));
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    max_bytes: usize,
    errors: &mut Vec<ValidationError>,
) {
    if !is_valid_identifier(value, max_bytes) {
        errors.push(ValidationError::new(
            field,
            format!("must be 1 to {max_bytes} ASCII letters, digits, '.', '_', ':', or '-'"),
        ));
    }
}

fn validate_absolute_normalized_path(path: &str, errors: &mut Vec<ValidationError>) {
    if !is_normalized_absolute_path(path) {
        errors.push(ValidationError::new(
            "capabilities.filesystem.path",
            "must be a normalized absolute path below the filesystem root",
        ));
    }
}

pub(crate) fn is_valid_identifier(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}

pub(crate) fn is_normalized_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        && path != "/"
        && path.len() <= MAX_PATH_BYTES
        && !path.contains('\0')
        && path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

pub(crate) fn is_valid_network_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host != host.to_ascii_lowercase() {
        return false;
    }
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }

    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicy, Budget, CapabilitySet, FileAccess, FileCapability, NetworkDestination,
        NetworkPolicy, NetworkTransport, TaskSpec,
    };

    fn destination(host: &str, port: u16) -> NetworkDestination {
        NetworkDestination {
            host: host.to_owned(),
            transport: NetworkTransport::Tcp,
            port,
        }
    }

    fn valid_task() -> TaskSpec {
        TaskSpec {
            idempotency_key: "repo-analysis-001".to_owned(),
            goal: "Analyze the repository and report test failures".to_owned(),
            capabilities: CapabilitySet {
                filesystem: vec![FileCapability {
                    path: "/workspace/project".to_owned(),
                    access: FileAccess::Read,
                }],
                network: NetworkPolicy::Deny,
                tools: vec!["test_runner".to_owned()],
            },
            budget: Budget {
                wall_time_seconds: 1_800,
                memory_bytes: 8_589_934_592,
                max_parallel_agents: 2,
            },
            approval: ApprovalPolicy {
                required_for: vec!["filesystem.write".to_owned(), "git.commit".to_owned()],
            },
        }
    }

    #[test]
    fn accepts_valid_local_only_task() {
        assert_eq!(valid_task().validate(), Ok(()));
    }

    #[test]
    fn published_example_matches_the_task_contract() {
        let json = include_str!("../../../examples/task.json");
        let task: TaskSpec = serde_json::from_str(json).expect("example must be valid JSON");

        assert_eq!(task.validate(), Ok(()));
    }

    #[test]
    fn rejects_blank_and_oversized_goals() {
        let mut blank = valid_task();
        blank.goal = " \n\t".to_owned();
        assert!(blank.validate().is_err());

        let mut oversized = valid_task();
        oversized.goal = "a".repeat(8_193);
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn rejects_path_traversal_root_and_relative_paths() {
        for path in [
            "/workspace/../secret",
            "/",
            "workspace/project",
            "/workspace//project",
        ] {
            let mut task = valid_task();
            task.capabilities.filesystem[0].path = path.to_owned();

            let errors = task.validate().expect_err("unsafe path must be rejected");
            assert!(
                errors
                    .errors()
                    .iter()
                    .any(|error| error.field() == "capabilities.filesystem.path")
            );
        }
    }

    #[test]
    fn rejects_zero_and_excessive_parallel_budgets() {
        let mut task = valid_task();
        task.budget.wall_time_seconds = 0;
        task.budget.memory_bytes = 0;
        task.budget.max_parallel_agents = 9;

        let errors = task
            .validate()
            .expect_err("invalid budgets must be rejected");
        assert_eq!(errors.errors().len(), 3);
    }

    #[test]
    fn network_defaults_to_deny_when_omitted() {
        let json = r#"
        {
          "idempotency_key": "task-001",
          "goal": "Inspect source files",
          "capabilities": {"filesystem": [], "tools": []},
          "budget": {
            "wall_time_seconds": 60,
            "memory_bytes": 1048576,
            "max_parallel_agents": 1
          },
          "approval": {"required_for": []}
        }
        "#;

        let task: TaskSpec = serde_json::from_str(json).expect("valid task JSON");
        assert!(matches!(task.capabilities.network, NetworkPolicy::Deny));
    }

    #[test]
    fn rejects_unknown_json_fields() {
        let json = r#"
        {
          "idempotency_key": "task-001",
          "goal": "Inspect source files",
          "capabilities": {"filesystem": [], "network": {"mode": "deny"}, "tools": []},
          "budget": {
            "wall_time_seconds": 60,
            "memory_bytes": 1048576,
            "max_parallel_agents": 1,
            "unlimited": true
          },
          "approval": {"required_for": []}
        }
        "#;

        assert!(serde_json::from_str::<TaskSpec>(json).is_err());
    }

    #[test]
    fn rejects_network_allow_without_destinations() {
        let mut task = valid_task();
        task.capabilities.network = NetworkPolicy::Allow {
            destinations: Vec::new(),
        };

        assert!(task.validate().is_err());
    }

    #[test]
    fn accepts_exact_tcp_destinations_and_ip_addresses() {
        let mut task = valid_task();
        task.capabilities.network = NetworkPolicy::Allow {
            destinations: vec![
                destination("api.example.com", 443),
                destination("127.0.0.1", 8080),
                destination("2001:db8::1", 8443),
            ],
        };

        assert_eq!(task.validate(), Ok(()));
    }

    #[test]
    fn rejects_schemes_paths_embedded_ports_and_invalid_host_names() {
        for host in [
            "https://example.com",
            "example.com/path",
            "example.com:443",
            "Example.com",
            "-invalid.example",
            "invalid..example",
        ] {
            let mut task = valid_task();
            task.capabilities.network = NetworkPolicy::Allow {
                destinations: vec![destination(host, 443)],
            };

            assert!(task.validate().is_err(), "host should be rejected: {host}");
        }
    }

    #[test]
    fn rejects_duplicate_capability_entries() {
        let mut task = valid_task();
        task.capabilities.tools = vec!["test_runner".to_owned(), "test_runner".to_owned()];
        task.approval.required_for = vec!["git.commit".to_owned(), "git.commit".to_owned()];
        task.capabilities.network = NetworkPolicy::Allow {
            destinations: vec![
                destination("example.com", 443),
                destination("example.com", 443),
            ],
        };

        let errors = task
            .validate()
            .expect_err("duplicate entries must be rejected");
        assert_eq!(
            errors
                .errors()
                .iter()
                .filter(|error| error.message().contains("duplicate"))
                .count(),
            3
        );
    }

    #[test]
    fn rejects_zero_ports_and_legacy_or_unknown_network_shapes() {
        let mut task = valid_task();
        task.capabilities.network = NetworkPolicy::Allow {
            destinations: vec![destination("api.example.com", 0)],
        };
        let errors = task.validate().expect_err("port zero must be rejected");
        assert!(
            errors
                .errors()
                .iter()
                .any(|error| error.field() == "capabilities.network.destinations.port")
        );

        let legacy = r#"{"mode":"allow","hosts":["api.example.com"]}"#;
        assert!(serde_json::from_str::<NetworkPolicy>(legacy).is_err());

        let unknown_transport = r#"
        {
          "mode": "allow",
          "destinations": [
            {"host": "api.example.com", "transport": "udp", "port": 443}
          ]
        }
        "#;
        assert!(serde_json::from_str::<NetworkPolicy>(unknown_transport).is_err());
    }

    #[test]
    fn allows_same_host_on_distinct_ports() {
        let mut task = valid_task();
        task.capabilities.network = NetworkPolicy::Allow {
            destinations: vec![
                destination("api.example.com", 443),
                destination("api.example.com", 8443),
            ],
        };

        assert_eq!(task.validate(), Ok(()));
    }

    #[test]
    fn errors_do_not_echo_sensitive_input() {
        let mut task = valid_task();
        task.goal = "".to_owned();
        task.idempotency_key = "secret value".to_owned();

        let errors = task.validate().expect_err("task must be invalid");
        let message = errors
            .errors()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");

        assert!(!message.contains("secret value"));
    }
}
