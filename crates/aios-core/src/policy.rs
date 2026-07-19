use serde::{Deserialize, Serialize};

use crate::task::{is_normalized_absolute_path, is_valid_identifier, is_valid_network_host};
use crate::{FileAccess, NetworkPolicy, NetworkTransport, TaskSpec, ValidationErrors};

const MAX_ACTION_BYTES: usize = 64;
const MAX_TOOL_NAME_BYTES: usize = 64;

/// A resource operation presented to the deterministic capability policy.
///
/// This type intentionally does not implement `Debug` or serialization because
/// resource values may contain sensitive paths or destinations.
pub enum CapabilityRequest<'a> {
    File {
        path: &'a str,
        access: FileAccess,
    },
    Network {
        host: &'a str,
        transport: NetworkTransport,
        port: u16,
    },
    Tool {
        tool: &'a str,
        action: &'a str,
    },
}

/// A resource-free authorization result safe for API responses and audit events.
#[must_use]
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny { reason: DenialReason },
    ApprovalRequired,
}

/// Stable, non-sensitive reason for a denied operation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DenialReason {
    InvalidRequest,
    CapabilityNotGranted,
}

/// Evaluates pre-execution operations against one validated Task boundary.
pub struct CapabilityPolicy<'a> {
    task: &'a TaskSpec,
}

impl<'a> CapabilityPolicy<'a> {
    /// Creates a policy only when the complete Task contract is valid.
    pub fn from_task(task: &'a TaskSpec) -> Result<Self, ValidationErrors> {
        task.validate()?;
        Ok(Self { task })
    }

    /// Returns a deterministic decision without including resource values.
    pub fn evaluate(&self, request: CapabilityRequest<'_>) -> PolicyDecision {
        match request {
            CapabilityRequest::File { path, access } => self.evaluate_file(path, access),
            CapabilityRequest::Network {
                host,
                transport,
                port,
            } => self.evaluate_network(host, transport, port),
            CapabilityRequest::Tool { tool, action } => self.evaluate_tool(tool, action),
        }
    }

    fn evaluate_file(&self, path: &str, access: FileAccess) -> PolicyDecision {
        if !is_normalized_absolute_path(path) {
            return invalid_request();
        }

        let granted = self.task.capabilities.filesystem.iter().any(|capability| {
            capability.access == access && path_is_within(path, &capability.path)
        });
        let action = match access {
            FileAccess::Read => "filesystem.read",
            FileAccess::Write => "filesystem.write",
        };
        self.finish(granted, action)
    }

    fn evaluate_network(
        &self,
        host: &str,
        transport: NetworkTransport,
        port: u16,
    ) -> PolicyDecision {
        if !is_valid_network_host(host) || port == 0 {
            return invalid_request();
        }

        let granted = match &self.task.capabilities.network {
            NetworkPolicy::Deny => false,
            NetworkPolicy::Allow { destinations } => destinations.iter().any(|allowed| {
                allowed.host == host && allowed.transport == transport && allowed.port == port
            }),
        };
        self.finish(granted, "network.egress")
    }

    fn evaluate_tool(&self, tool: &str, action: &str) -> PolicyDecision {
        if !is_valid_identifier(tool, MAX_TOOL_NAME_BYTES)
            || !is_valid_identifier(action, MAX_ACTION_BYTES)
        {
            return invalid_request();
        }

        let granted = self
            .task
            .capabilities
            .tools
            .iter()
            .any(|allowed| allowed == tool);
        self.finish(granted, action)
    }

    fn finish(&self, granted: bool, action: &str) -> PolicyDecision {
        if !granted {
            return PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            };
        }
        if self
            .task
            .approval
            .required_for
            .iter()
            .any(|required| required == action)
        {
            return PolicyDecision::ApprovalRequired;
        }
        PolicyDecision::Allow
    }
}

fn invalid_request() -> PolicyDecision {
    PolicyDecision::Deny {
        reason: DenialReason::InvalidRequest,
    }
}

fn path_is_within(path: &str, capability_path: &str) -> bool {
    path == capability_path
        || path
            .strip_prefix(capability_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use crate::{
        ApprovalPolicy, Budget, CapabilitySet, FileAccess, FileCapability, NetworkDestination,
        NetworkPolicy, NetworkTransport, TaskSpec,
    };

    use super::{CapabilityPolicy, CapabilityRequest, DenialReason, PolicyDecision};

    fn task() -> TaskSpec {
        TaskSpec {
            idempotency_key: "policy-test-001".to_owned(),
            goal: "Evaluate a capability request".to_owned(),
            capabilities: CapabilitySet {
                filesystem: vec![
                    FileCapability {
                        path: "/workspace/project".to_owned(),
                        access: FileAccess::Read,
                    },
                    FileCapability {
                        path: "/workspace-output".to_owned(),
                        access: FileAccess::Write,
                    },
                ],
                network: NetworkPolicy::Allow {
                    destinations: vec![
                        NetworkDestination {
                            host: "api.example.com".to_owned(),
                            transport: NetworkTransport::Tcp,
                            port: 443,
                        },
                        NetworkDestination {
                            host: "127.0.0.1".to_owned(),
                            transport: NetworkTransport::Tcp,
                            port: 8080,
                        },
                    ],
                },
                tools: vec!["git".to_owned(), "test_runner".to_owned()],
            },
            budget: Budget {
                wall_time_seconds: 60,
                memory_bytes: 1_048_576,
                max_parallel_agents: 1,
            },
            approval: ApprovalPolicy {
                required_for: vec![
                    "filesystem.write".to_owned(),
                    "git.commit".to_owned(),
                    "network.egress".to_owned(),
                ],
            },
        }
    }

    #[test]
    fn allows_exact_and_descendant_read_paths() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        for path in ["/workspace/project", "/workspace/project/src/lib.rs"] {
            assert_eq!(
                policy.evaluate(CapabilityRequest::File {
                    path,
                    access: FileAccess::Read,
                }),
                PolicyDecision::Allow
            );
        }
    }

    #[test]
    fn denies_sibling_prefix_and_ungranted_access_mode() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        for request in [
            CapabilityRequest::File {
                path: "/workspace/project-private/secret",
                access: FileAccess::Read,
            },
            CapabilityRequest::File {
                path: "/workspace/project/src/lib.rs",
                access: FileAccess::Write,
            },
            CapabilityRequest::File {
                path: "/workspace-output/result.txt",
                access: FileAccess::Read,
            },
        ] {
            assert_eq!(
                policy.evaluate(request),
                PolicyDecision::Deny {
                    reason: DenialReason::CapabilityNotGranted,
                }
            );
        }
    }

    #[test]
    fn rejects_invalid_paths_before_capability_matching() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        for path in [
            "workspace/project",
            "/workspace/project/../secret",
            "/workspace//project",
            "/",
        ] {
            assert_eq!(
                policy.evaluate(CapabilityRequest::File {
                    path,
                    access: FileAccess::Read,
                }),
                PolicyDecision::Deny {
                    reason: DenialReason::InvalidRequest,
                }
            );
        }
    }

    #[test]
    fn requires_approval_only_after_capability_is_granted() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        assert_eq!(
            policy.evaluate(CapabilityRequest::File {
                path: "/workspace-output/result.txt",
                access: FileAccess::Write,
            }),
            PolicyDecision::ApprovalRequired
        );
        assert_eq!(
            policy.evaluate(CapabilityRequest::File {
                path: "/private/result.txt",
                access: FileAccess::Write,
            }),
            PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            }
        );
    }

    #[test]
    fn network_policy_uses_exact_validated_destinations() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        for (host, port) in [("api.example.com", 443), ("127.0.0.1", 8080)] {
            assert_eq!(
                policy.evaluate(CapabilityRequest::Network {
                    host,
                    transport: NetworkTransport::Tcp,
                    port,
                }),
                PolicyDecision::ApprovalRequired
            );
        }
        assert_eq!(
            policy.evaluate(CapabilityRequest::Network {
                host: "sub.api.example.com",
                transport: NetworkTransport::Tcp,
                port: 443,
            }),
            PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            }
        );
        assert_eq!(
            policy.evaluate(CapabilityRequest::Network {
                host: "api.example.com",
                transport: NetworkTransport::Tcp,
                port: 80,
            }),
            PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            }
        );
        for host in [
            "API.EXAMPLE.COM",
            "https://api.example.com",
            "api.example.com:443",
        ] {
            assert_eq!(
                policy.evaluate(CapabilityRequest::Network {
                    host,
                    transport: NetworkTransport::Tcp,
                    port: 443,
                }),
                PolicyDecision::Deny {
                    reason: DenialReason::InvalidRequest,
                }
            );
        }
        assert_eq!(
            policy.evaluate(CapabilityRequest::Network {
                host: "api.example.com",
                transport: NetworkTransport::Tcp,
                port: 0,
            }),
            PolicyDecision::Deny {
                reason: DenialReason::InvalidRequest,
            }
        );
    }

    #[test]
    fn deny_network_policy_is_fail_closed() {
        let mut task = task();
        task.capabilities.network = NetworkPolicy::Deny;
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        assert_eq!(
            policy.evaluate(CapabilityRequest::Network {
                host: "api.example.com",
                transport: NetworkTransport::Tcp,
                port: 443,
            }),
            PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            }
        );
    }

    #[test]
    fn tool_capability_and_action_approval_are_independent() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");

        assert_eq!(
            policy.evaluate(CapabilityRequest::Tool {
                tool: "git",
                action: "git.status",
            }),
            PolicyDecision::Allow
        );
        assert_eq!(
            policy.evaluate(CapabilityRequest::Tool {
                tool: "git",
                action: "git.commit",
            }),
            PolicyDecision::ApprovalRequired
        );
        assert_eq!(
            policy.evaluate(CapabilityRequest::Tool {
                tool: "shell",
                action: "git.commit",
            }),
            PolicyDecision::Deny {
                reason: DenialReason::CapabilityNotGranted,
            }
        );
    }

    #[test]
    fn invalid_tool_request_is_denied_without_echoing_values() {
        let task = task();
        let policy = CapabilityPolicy::from_task(&task).expect("valid policy");
        let decision = policy.evaluate(CapabilityRequest::Tool {
            tool: "git;secret-value",
            action: "git.commit",
        });

        assert_eq!(
            decision,
            PolicyDecision::Deny {
                reason: DenialReason::InvalidRequest,
            }
        );
        let serialized = serde_json::to_string(&decision).expect("serialize decision");
        assert!(!serialized.contains("secret-value"));
        assert_eq!(
            serialized,
            r#"{"decision":"deny","reason":"INVALID_REQUEST"}"#
        );
    }

    #[test]
    fn refuses_to_build_policy_from_invalid_task() {
        let mut task = task();
        task.capabilities.filesystem[0].path = "/workspace/../secret".to_owned();

        assert!(CapabilityPolicy::from_task(&task).is_err());
    }
}
