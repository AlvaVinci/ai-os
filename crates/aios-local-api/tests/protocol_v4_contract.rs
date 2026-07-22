#![cfg(unix)]

use aios_local_api::{
    ApiRequest, ApiResponse, ApiService, MAX_SUPPORTED_PROTOCOL_VERSION,
    MIN_SUPPORTED_PROTOCOL_VERSION, PROTOCOL_VERSION,
};
use aios_runtime::{InMemoryEventStore, TaskSupervisor};
use serde::Deserialize;
use serde_json::{Value, json};

const HEALTH_REQUEST: &str = include_str!("fixtures/v4/health-request.json");
const HEALTH_RESPONSE: &str = include_str!("fixtures/v4/health-response.json");

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PreviousVersionFourResult {
    Healthy,
}

#[test]
fn version_four_health_contract_matches_the_golden_fixtures() {
    let request: ApiRequest = serde_json::from_str(HEALTH_REQUEST).expect("valid request fixture");
    let mut service = ApiService::new(TaskSupervisor::new(InMemoryEventStore::default()));
    let response = service.handle(request);

    let actual = serde_json::to_value(response).expect("serialize response");
    let expected: Value = serde_json::from_str(HEALTH_RESPONSE).expect("valid response fixture");
    assert_eq!(actual, expected);
}

#[test]
fn version_four_rejects_unknown_request_fields() {
    let request = json!({
        "protocol_version": 4,
        "request": {
            "method": "health",
            "future_option": true
        }
    });

    assert!(serde_json::from_value::<ApiRequest>(request).is_err());
}

#[test]
fn version_four_rejects_duplicate_request_fields() {
    let request = r#"
        {
          "protocol_version": 4,
          "protocol_version": 4,
          "request": {"method": "health"}
        }
    "#;

    assert!(serde_json::from_str::<ApiRequest>(request).is_err());
}

#[test]
fn version_four_clients_ignore_additive_response_object_fields() {
    let response = json!({
        "protocol_version": 4,
        "status": "ok",
        "future_envelope_field": "ignored",
        "result": {
            "type": "healthy",
            "future_result_field": "ignored",
            "supported_protocol_versions": {
                "minimum": 4,
                "maximum": 4
            }
        }
    });

    serde_json::from_value::<ApiResponse>(response).expect("additive response fields are ignored");
}

#[test]
fn previous_version_four_health_result_ignores_the_added_support_window() {
    let response: Value = serde_json::from_str(HEALTH_RESPONSE).expect("valid response fixture");
    let result = response
        .get("result")
        .cloned()
        .expect("health result object");

    assert!(matches!(
        serde_json::from_value::<PreviousVersionFourResult>(result)
            .expect("previous Version 4 result remains decodable"),
        PreviousVersionFourResult::Healthy
    ));
}

#[test]
fn published_supported_window_is_exactly_version_four() {
    assert_eq!(MIN_SUPPORTED_PROTOCOL_VERSION, 4);
    assert_eq!(MAX_SUPPORTED_PROTOCOL_VERSION, 4);
    assert_eq!(PROTOCOL_VERSION, 4);
}
