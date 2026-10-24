//! Where an absent `Scope` is accepted and where it is still refused
//! (ledger 762, route (c)).
//!
//! **THE THREE READS ACCEPT ONE; THE WRITES DO NOT.** No read filters, keys or
//! partitions on a `Scope` field, and D67's id has a header fallback, so a read
//! has no use for one. `RegisterProject` and `SyncProjects` stamp
//! `created_by` and `updated_by` from `Scope.user_id`, and `RegisterProject`
//! keys D9's idempotency ledger on it. Ledger 762 relaxed the reads only, so
//! every write — `TouchProjects` included — still refuses an absent one. `ListProjects` and
//! `ResolveProject` are pinned in `tests/list.rs` and `tests/resolve.rs`;
//! `GetProject` and the writes are pinned here.

mod support;
use support::*;

use tonic::{Code, Request};
use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
use yadgar_project_db::pb::yadgar::project::v1::{
    GetProjectRequest, RegisterProjectRequest, SyncProjectsRequest, TouchProjectsRequest,
};

/// A private path in `U1`'s own account, so a registration of it is refused
/// for no reason but the absent `Scope`.
const PRIVATE: &str = "local/0192f3a4-5b6c-7d8e-9f01-23456789abc1/scope-probe";

/// `GetProject` is a read like the other two: `src/read.rs`'s `get` bound the
/// `Scope` as `_scope` and never read it. It has no caller today (`project`
/// does not expose it), and requiring a `Scope` here would leave the same
/// wall ledger 762 removed waiting for the first one.
///
/// **THE MUTATION THAT REDS IT:** restore `scope_of(&req.scope)?` in
/// `src/read.rs`'s `get`.
#[tokio::test]
async fn get_accepts_an_absent_scope() {
    let w = World::fresh("project_db_scope_get").await;
    w.register(A).await;

    let got =
        w.db.get_project(Request::new(GetProjectRequest {
            scope: None,
            path: A.into(),
        }))
        .await
        .expect("an absent scope is accepted on a read")
        .into_inner()
        .project
        .expect("the project");
    assert_eq!(got.path, A);
}

/// **THE WRITES STILL REFUSE AN ABSENT `Scope`, AND WRITE NOTHING.**
/// `RegisterProject` and `TouchProjects` refuse through `sql::scope_of`, the
/// first after its caller is admitted;
/// `SyncProjects` through its own check, after the caller is admitted. Each
/// refusal opens "scope is required", so it is the absent `Scope` that refused
/// it and not a later check on one of its fields (`Class::of` refuses an empty
/// `user_id` with a message that also mentions the scope).
///
/// **THE MUTATION THAT REDS IT:** delete the `scope_of` call in
/// `src/write/register.rs` or `src/write/touch.rs`, or the presence check in
/// `src/write/sync.rs`.
#[tokio::test]
async fn the_writes_still_refuse_an_absent_scope() {
    let w = World::fresh("project_db_scope_writes").await;
    w.register(A).await;
    let rows = w.project_count().await;

    let register =
        w.db.register_as(
            Some(yadgar_project_db::service::REGISTER_CALLER),
            RegisterProjectRequest {
                idempotency: None,
                scope: None,
                path: PRIVATE.into(),
                display_name: "Probe".into(),
                source_repo: String::new(),
            },
            yadgar_dial::Deadline::none(),
        )
        .await
        .expect_err("a registration stamps created_by from the scope");
    assert_eq!(register.code(), Code::InvalidArgument, "{register:?}");
    assert!(
        register.message().starts_with("scope is required"),
        "{register:?}"
    );

    let touch =
        w.db.touch_projects(Request::new(TouchProjectsRequest {
            scope: None,
            paths: vec![A.to_string()],
            seen_at: Some(prost_types::Timestamp {
                seconds: 1_700_000_000,
                nanos: 0,
            }),
        }))
        .await
        .expect_err("a touch is a write");
    assert_eq!(touch.code(), Code::InvalidArgument, "{touch:?}");
    assert!(
        touch.message().starts_with("scope is required"),
        "{touch:?}"
    );

    let sync =
        w.db.sync_as(
            Some(yadgar_project_db::service::SYNC_CALLER),
            SyncProjectsRequest {
                scope: None,
                ..w.sync_request(&[declared(ROOT, "", FIXTURE_REPO)], &[], false)
            },
            yadgar_dial::Deadline::none(),
        )
        .await
        .expect_err("a sync stamps created_by from the scope");
    assert_eq!(sync.code(), Code::InvalidArgument, "{sync:?}");
    assert!(sync.message().starts_with("scope is required"), "{sync:?}");

    assert_eq!(
        w.project_count().await,
        rows,
        "a refused write wrote no row"
    );
    assert_eq!(
        w.stored_last_seen_at(A).await,
        None,
        "the touch moved nothing"
    );
}
