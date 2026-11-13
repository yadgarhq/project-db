//! AC2 (guard): the D67 `CallRecord` of `SyncProjects` and `ResolveProject`
//! carries the CAPPED request id, byte-identical to before the shared cap, for
//! both sources (the `x-yadgar-request-id` header and `Scope.request_id`).
//!
//! DB-free: the handlers run on a lazy pool that points nowhere. `SyncProjects`
//! is refused PERMISSION_DENIED (no peer certificate) before any database
//! access; `ResolveProject` fails in the store. Either way one record is
//! emitted, and only its `request_id` is pinned.
//!
//! Spec: "Behaviour must be byte-identical, or records from mixed versions stop
//! joining during rollout."

mod support;
use support::*;

use std::time::Duration;

use tonic::Request;
use yadgar_project_db::pb::yadgar::common::v1::Scope;
use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
use yadgar_project_db::pb::yadgar::project::v1::{ResolveProjectRequest, SyncProjectsRequest};
use yadgar_project_db::service::ProjectDb;

/// A pool that points nowhere: any statement fails.
fn nowhere() -> sqlx::MySqlPool {
    sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/nowhere")
        .unwrap()
}

fn sync_with_scope(request_id: Option<&str>) -> SyncProjectsRequest {
    SyncProjectsRequest {
        scope: request_id.map(|id| Scope {
            request_id: id.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn resolve_with_scope(request_id: Option<&str>) -> ResolveProjectRequest {
    ResolveProjectRequest {
        scope: request_id.map(|id| Scope {
            request_id: id.into(),
            ..Default::default()
        }),
        candidate_path: "acme/x".into(),
    }
}

fn cut(c: char, n: usize) -> String {
    format!("{}...", c.to_string().repeat(n))
}

/// The one record `SyncProjects` emits for a header-only call.
async fn sync_header_record(header: &str) -> String {
    the_call_record(
        ProjectDb::new(nowhere()).sync_projects(with_request_id(sync_with_scope(None), header)),
    )
    .await
}

/// The one record `SyncProjects` emits for a Scope-only call.
async fn sync_scope_record(id: &str) -> String {
    the_call_record(
        ProjectDb::new(nowhere()).sync_projects(Request::new(sync_with_scope(Some(id)))),
    )
    .await
}

#[tokio::test]
async fn sync_header_over_the_cap_is_cut_to_256_and_a_marker() {
    // AC2: header 'x' * 1000 -> first 256 chars + '...', refusal kept.
    let record = sync_header_record(&"x".repeat(1000)).await;
    assert!(carries(&record, &cut('x', 256)), "{record}");
    assert!(record.contains("PERMISSION_DENIED"), "{record}");

    // AC2 triangulation: other char and length, 'k' * 300.
    let record = sync_header_record(&"k".repeat(300)).await;
    assert!(carries(&record, &cut('k', 256)), "{record}");
    assert!(record.contains("PERMISSION_DENIED"), "{record}");
}

#[tokio::test]
async fn sync_header_at_the_cap_is_unchanged() {
    // AC2 boundary: exactly 256 chars -> unchanged, no marker.
    let id = "x".repeat(256);
    let record = sync_header_record(&id).await;
    assert!(carries(&record, &id), "{record}");

    // AC2 triangulation: 'm' * 256 and one char limit+1 shows the cut.
    let id = "m".repeat(256);
    let record = sync_header_record(&id).await;
    assert!(carries(&record, &id), "{record}");
    let record = sync_header_record(&"m".repeat(257)).await;
    assert!(carries(&record, &cut('m', 256)), "{record}");
}

#[tokio::test]
async fn sync_header_already_capped_is_unchanged() {
    // AC2 idempotent: 256 + '...' (259 chars) stays the same 259-char value.
    let id = cut('x', 256);
    assert_eq!(id.len(), 259);
    let record = sync_header_record(&id).await;
    assert!(carries(&record, &id), "{record}");

    // AC2 triangulation: same with 'j'.
    let id = cut('j', 256);
    let record = sync_header_record(&id).await;
    assert!(carries(&record, &id), "{record}");
}

#[tokio::test]
async fn sync_scope_request_id_over_the_cap_is_cut() {
    // AC2: Scope request_id 's' * 1000, no header.
    let record = sync_scope_record(&"s".repeat(1000)).await;
    assert!(carries(&record, &cut('s', 256)), "{record}");

    // AC2 triangulation: 'w' * 400.
    let record = sync_scope_record(&"w".repeat(400)).await;
    assert!(carries(&record, &cut('w', 256)), "{record}");

    // AC2 boundary: Scope id of exactly 256 is unchanged.
    let id = "s".repeat(256);
    let record = sync_scope_record(&id).await;
    assert!(carries(&record, &id), "{record}");
}

#[tokio::test]
async fn resolve_scope_request_id_over_the_cap_is_cut() {
    // AC2: Scope request_id 'r' * 1000, candidate_path acme/x.
    let record = the_call_record(
        ProjectDb::new(nowhere())
            .resolve_project(Request::new(resolve_with_scope(Some(&"r".repeat(1000))))),
    )
    .await;
    assert!(carries(&record, &cut('r', 256)), "{record}");

    // AC2 triangulation: 'u' * 500.
    let record = the_call_record(
        ProjectDb::new(nowhere())
            .resolve_project(Request::new(resolve_with_scope(Some(&"u".repeat(500))))),
    )
    .await;
    assert!(carries(&record, &cut('u', 256)), "{record}");
}

#[tokio::test]
async fn resolve_header_request_id_over_the_cap_is_cut() {
    // AC2: no Scope, header 'q' * 1000.
    let record = the_call_record(
        ProjectDb::new(nowhere())
            .resolve_project(with_request_id(resolve_with_scope(None), &"q".repeat(1000))),
    )
    .await;
    assert!(carries(&record, &cut('q', 256)), "{record}");

    // AC2 triangulation: 'v' * 257 (limit + 1).
    let record = the_call_record(
        ProjectDb::new(nowhere())
            .resolve_project(with_request_id(resolve_with_scope(None), &"v".repeat(257))),
    )
    .await;
    assert!(carries(&record, &cut('v', 256)), "{record}");
}
