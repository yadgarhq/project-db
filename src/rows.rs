//! One row, one `Project` — and the reason every timestamp here is an integer.
//!
//! **NO RUST TYPE IN THIS TREE DECODES A MariaDB `TIMESTAMP`.** `sqlx` is
//! compiled with neither the `chrono` nor the `time` feature, so `try_get` on a
//! `TIMESTAMP` column has no target type to land in and fails at DECODE — which
//! is a runtime error in a query that compiled perfectly. The contract carries
//! four of them (`last_seen_at` and three on `Meta`), so this is not a corner to
//! route around.
//!
//! The answer is to make the ENGINE do the conversion: `UNIX_TIMESTAMP` yields a
//! number and `CAST(... AS SIGNED)` makes it a `BIGINT`, which decodes into
//! `i64` like any other integer. `UNIX_TIMESTAMP(NULL)` is NULL, so a column
//! that may be absent decodes into `Option<i64>` and absence survives the trip
//! — which matters for `last_seen_at`, where "nothing has ever run under this
//! project" and "something ran at the epoch" are different facts.
//!
//! **THE CAST LIVES IN [`COLUMNS`], WHICH IS WHY THAT CONSTANT EXISTS.** A
//! statement that selects a bare `created_at` compiles, runs, and fails when the
//! row is read. Naming the column list once means the two read paths cannot
//! drift into selecting different things, and [`row_to_project`] cannot ask for
//! a name no statement produced.

use prost_types::Timestamp;
use sqlx::Row;
use tonic::Status;

use crate::pb::yadgar::common::v1::Meta;
use crate::pb::yadgar::project::v1::Project;
use crate::sql::internal;

/// Every column a `Project` is built from, with the three timestamps already
/// converted by the engine.
///
/// The aliases end in `_secs` so that a reader of [`row_to_project`] can see at
/// the call site that the value is an epoch second and not a timestamp type.
pub const COLUMNS: &str = "id, version, path, display_name, status, created_by, updated_by, \
     CAST(UNIX_TIMESTAMP(created_at)   AS SIGNED) AS created_at_secs, \
     CAST(UNIX_TIMESTAMP(updated_at)   AS SIGNED) AS updated_at_secs, \
     CAST(UNIX_TIMESTAMP(last_seen_at) AS SIGNED) AS last_seen_at_secs";

/// An epoch second as the contract's timestamp.
///
/// `nanos: 0` is the honest value rather than a rounding: a MariaDB `TIMESTAMP`
/// declared without a fractional-seconds precision stores whole seconds, so
/// there is no sub-second component being discarded here.
pub fn at(seconds: i64) -> Timestamp {
    Timestamp { seconds, nanos: 0 }
}

/// The whole row.
///
/// **`Meta` IS CONSTRUCTED EXHAUSTIVELY, NEVER WITH `..Default::default()`.**
/// The rest pattern would compile for ever, silently leaving any field the
/// contract later adds at its zero value — and `Meta` is the envelope every
/// consumer reads. Spelling every field out makes a contract bump a compile
/// error somebody has to answer, which is the only moment the question gets
/// asked.
///
/// **`owner_user_id` IS DELIBERATELY EMPTY, AND SO ARE `team_id` AND
/// `visibility`.** D12's ladder is about who may see a RECORD, and a project is
/// not a record in a project — it is the partition key itself. `Project` carries
/// no visibility field, which is the contract saying the registry is not
/// access-controlled row by row (see [`crate::service`]). Writing the registrant
/// into `owner_user_id` would invent an owner nothing enforces, and ADR-0512 is
/// this estate's recorded cost of a field that reads as an identity and is not
/// one. `created_by` already carries who registered it, which is the true
/// statement.
///
/// `aliases` is EMPTY here and filled by the caller. A project's aliases live in
/// another table, so they are fetched per page rather than per row — see
/// [`crate::read`]. Leaving the field empty and unfilled is exactly how a
/// caller's data vanishes without a word, so no path returns a `Project` from
/// this function without going through `crate::read::with_aliases`.
pub fn row_to_project(row: &sqlx::mysql::MySqlRow) -> Result<Project, Status> {
    let secs = |name| row.try_get::<Option<i64>, _>(name).map_err(internal);
    Ok(Project {
        meta: Some(Meta {
            id: row.try_get("id").map_err(internal)?,
            version: row.try_get("version").map_err(internal)?,
            // A project's own partition key is its own path. `Meta.project_id`
            // is "which project does this record belong to", and the answer for
            // a project is itself.
            project_id: row.try_get("path").map_err(internal)?,
            owner_user_id: String::new(),
            team_id: String::new(),
            visibility: 0,
            created_by: row.try_get("created_by").map_err(internal)?,
            updated_by: row.try_get("updated_by").map_err(internal)?,
            created_at: secs("created_at_secs")?.map(at),
            updated_at: secs("updated_at_secs")?.map(at),
            // Absent, always. D26's soft delete is not this module's lifecycle:
            // a project is ARCHIVED, which is a status the contract declares,
            // and archiving is not deletion — every record ever written under
            // the path still carries it.
            deleted_at: None,
            derived_from: Vec::new(),
        }),
        path: row.try_get("path").map_err(internal)?,
        display_name: row.try_get("display_name").map_err(internal)?,
        status: row.try_get::<i8, _>("status").map_err(internal)? as i32,
        last_seen_at: secs("last_seen_at_secs")?.map(at),
        aliases: Vec::new(),
    })
}
