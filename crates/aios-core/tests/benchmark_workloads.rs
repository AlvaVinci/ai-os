use std::collections::BTreeSet;

use aios_core::{FileAccess, NetworkPolicy, TaskSpec};

const REPOSITORY_INVESTIGATION: &str =
    include_str!("../../../benchmarks/workloads/repository-investigation.json");
const TEST_FAILURE_DIAGNOSIS: &str =
    include_str!("../../../benchmarks/workloads/test-failure-diagnosis.json");
const DEPENDENCY_ADVISORY_REVIEW: &str =
    include_str!("../../../benchmarks/workloads/dependency-advisory-review.json");

fn parse(json: &str) -> TaskSpec {
    let task: TaskSpec = serde_json::from_str(json).expect("benchmark must be valid JSON");
    task.validate().expect("benchmark Task must validate");
    task
}

#[test]
fn published_benchmark_workloads_match_the_task_contract() {
    let mut idempotency_keys = BTreeSet::new();
    for json in [
        REPOSITORY_INVESTIGATION,
        TEST_FAILURE_DIAGNOSIS,
        DEPENDENCY_ADVISORY_REVIEW,
    ] {
        let task = parse(json);
        assert!(
            idempotency_keys.insert(task.idempotency_key),
            "benchmark idempotency prefixes must be unique"
        );
    }
}

#[test]
fn repository_investigation_is_read_only_and_offline() {
    let task = parse(REPOSITORY_INVESTIGATION);

    assert_eq!(task.capabilities.filesystem.len(), 1);
    assert_eq!(task.capabilities.filesystem[0].path, "/workspace/project");
    assert_eq!(task.capabilities.filesystem[0].access, FileAccess::Read);
    assert!(matches!(task.capabilities.network, NetworkPolicy::Deny));
    assert_eq!(task.capabilities.tools.as_slice(), ["source_search"]);
    assert!(task.approval.required_for.is_empty());
}

#[test]
fn test_diagnosis_limits_writes_and_requires_execution_approval() {
    let task = parse(TEST_FAILURE_DIAGNOSIS);

    assert!(task.capabilities.filesystem.iter().any(|capability| {
        capability.path == "/workspace/project" && capability.access == FileAccess::Read
    }));
    assert!(task.capabilities.filesystem.iter().any(|capability| {
        capability.path == "/workspace/project/target" && capability.access == FileAccess::Write
    }));
    assert!(!task.capabilities.filesystem.iter().any(|capability| {
        capability.path == "/workspace/project" && capability.access == FileAccess::Write
    }));
    assert!(matches!(task.capabilities.network, NetworkPolicy::Deny));
    assert_eq!(task.capabilities.tools.as_slice(), ["test_runner"]);
    assert_eq!(
        task.approval.required_for.as_slice(),
        ["test.run", "filesystem.write"]
    );
}

#[test]
fn dependency_review_scopes_egress_and_requires_network_approval() {
    let task = parse(DEPENDENCY_ADVISORY_REVIEW);

    let NetworkPolicy::Allow { hosts } = task.capabilities.network else {
        panic!("dependency review must use an explicit network allowlist");
    };
    assert_eq!(hosts.as_slice(), ["api.osv.dev"]);
    assert_eq!(task.capabilities.tools.as_slice(), ["dependency_scanner"]);
    assert_eq!(task.approval.required_for.as_slice(), ["network.egress"]);
    assert!(
        task.capabilities
            .filesystem
            .iter()
            .all(|capability| capability.access == FileAccess::Read)
    );
}
