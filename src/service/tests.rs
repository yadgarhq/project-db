//! What [`super::envelope`] hands the telemetry crate (yaadgaar #167).
//!
//! `Outcome.payload` is a rendering of what a handler returned. The telemetry
//! crate (v0.2.0) only MEASURES it — words from the text, bytes from
//! `encoded_bytes` — and the payload text never reaches the record. (The
//! record does carry the caller's `scope.user_id`; that is a separate field
//! and not this one.) These tests hold the stronger property anyway: no owner
//! id or other user id from the response enters the payload, so a later
//! telemetry change that ships the text cannot leak one. `Meta` carries three
//! of them (`owner_user_id`, `created_by`, `updated_by`).
//!
//! **THE MUTATION THAT REDS EVERY CASE:** render the response with `{r:?}`,
//! which is what `envelope` did before.

use super::envelope;
use crate::pb::yadgar::common::v1::Meta;
use crate::pb::yadgar::project::v1::*;

/// Three distinct user ids, so a rendering that drops one field and keeps
/// another is still caught. The shared middle is what a substring check finds.
const OWNER: &str = "yadgar:user:0192f3c1-envelope-owner-must-not-appear";
const CREATOR: &str = "yadgar:user:0192f3c1-envelope-creator-must-not-appear";
const UPDATER: &str = "yadgar:user:0192f3c1-envelope-updater-must-not-appear";
const LEAKED: &str = "must-not-appear";

const PRIVATE: &str = "local/0192f3c1-the-owner/notes";
const ORG: &str = "okapi-3e9d/widgets";

fn meta(path: &str) -> Meta {
    Meta {
        id: "yadgar:project:0192f3c1-the-row".into(),
        version: 3,
        project_id: path.into(),
        owner_user_id: OWNER.into(),
        created_by: CREATOR.into(),
        updated_by: UPDATER.into(),
        ..Default::default()
    }
}

fn project(path: &str) -> Project {
    Project {
        meta: Some(meta(path)),
        path: path.into(),
        display_name: "a display name".into(),
        ..Default::default()
    }
}

fn assert_redacted(rpc: &str, payload: &str, paths: &[&str]) {
    assert!(!payload.contains(LEAKED), "{rpc}: {payload}");
    assert!(!payload.contains("yadgar:user:"), "{rpc}: {payload}");
    for p in paths {
        assert!(payload.contains(p), "{rpc} keeps the path {p}: {payload}");
    }
}

#[test]
fn the_meta_responses_carry_no_user_id() {
    let register = RegisterProjectResponse {
        meta: Some(meta(PRIVATE)),
    };
    let rename = RenameProjectResponse {
        meta: Some(meta(PRIVATE)),
    };
    let archive = ArchiveProjectResponse {
        meta: Some(meta(PRIVATE)),
    };
    assert_redacted("Register", &envelope(&register, 1).payload, &[PRIVATE]);
    assert_redacted("Rename", &envelope(&rename, 1).payload, &[PRIVATE]);
    assert_redacted("Archive", &envelope(&archive, 1).payload, &[PRIVATE]);
}

#[test]
fn get_carries_no_user_id() {
    let get = GetProjectResponse {
        project: Some(project(PRIVATE)),
    };
    assert_redacted("Get", &envelope(&get, 1).payload, &[PRIVATE]);
}

#[test]
fn list_carries_no_user_id_and_keeps_every_path() {
    let list = ListProjectsResponse {
        projects: vec![project(PRIVATE), project(ORG)],
        next_page_token: String::new(),
    };
    let outcome = envelope(&list, 2);
    assert_redacted("List", &outcome.payload, &[PRIVATE, ORG]);
    assert_eq!(outcome.rows, 2);
}

/// The responses with no user id field still go through the same door, so a
/// field added to one later is not rendered by default.
#[test]
fn the_other_responses_render_their_own_fields() {
    let resolve = ResolveProjectResponse {
        resolved_path: ORG.into(),
        exact: true,
        ..Default::default()
    };
    assert_redacted("Resolve", &envelope(&resolve, 1).payload, &[ORG]);

    let sync = SyncProjectsResponse {
        inserted: 4,
        updated: 1,
        ..Default::default()
    };
    let payload = envelope(&sync, 5).payload;
    assert!(payload.contains("inserted=4"), "{payload}");
    assert!(payload.contains("updated=1"), "{payload}");

    assert!(!envelope(&TouchProjectsResponse {}, 0).payload.is_empty());
}
