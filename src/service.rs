//! `ProjectDbService`: the RPC surface, its instrumentation, and the two things
//! this module deliberately does NOT do with a `Scope`.
//!
//! One RPC is one transaction (D5). The operations themselves live next door —
//! `read` for the three that answer questions, `write` for the five that change
//! something — because what a handler does HERE is start a `Call`, hand off, and
//! record what came back.
//!
//! # `Scope` does not filter a row here, and that is a decision
//!
//! Every other `-db` in this estate turns `Scope` into a `WHERE` clause. This one
//! cannot, and the reason is not a policy choice but an impossibility:
//! **`ResolveProject` is the caller asking WHICH project a candidate path belongs
//! to, and `Scope.project_id` is derived from that same candidate.** Filtering the
//! resolution by it could only ever return what the caller already assumed —
//! `quinyx/qwfm/forecast` would resolve to itself or to nothing, the ancestor walk
//! D52 requires would be unreachable, and the rpc would answer no question.
//!
//! The contract says the same thing from the other side. `ListProjects` carries
//! its OWN subtree parameter, `under_path`; if `Scope.project_id` were the
//! subtree filter, that field would be redundant on the one rpc that has it.
//! `Project` does carry an owner: `Meta.owner_user_id` is the registrant of a
//! private (`local/...`) row and empty on an org row. That owner WILL BE
//! enforced at the GATEWAY (`plans/project-membership.md`, ADR-0947): car P2
//! will relay it through `project`'s `ListProjects`, car P3 will add the
//! gateway's membership check on calls made INTO a project, and that check will
//! refuse only under Enforcing (ADR-0841). It is never a rule here on who may
//! read the registry. This store makes no per-row decision against it, and
//! still reports no `team_id` and no `visibility`.
//!
//! **SO THE REGISTRY IS READABLE DEPLOYMENT-WIDE, AND THAT IS STATED RATHER THAN
//! LEFT TO BE INFERRED FROM CODE THAT HAPPENS NOT TO FILTER.** One deployment is
//! one organisation (D27), and what this store holds is the set of names that
//! organisation has registered — not the content filed under them. A caller who
//! can reach this service can list every project path in the installation. If
//! that is ever wrong, it is wrong at the level of who may reach the service, and
//! the change is a contract change rather than a predicate quietly added here.
//!
//! **SO A READ ACCEPTS AN ABSENT `Scope`; A WRITE STILL REFUSES ONE (ledger
//! 762, route (c) of `plans/the-project-logic-tier.md` §6).** `ResolveProject`,
//! `GetProject` and `ListProjects` read no field of it. The writes keep the
//! requirement: `RegisterProject` and `SyncProjects` stamp `user_id` as
//! `created_by` and `updated_by`, `RegisterProject` keys D9's idempotency
//! ledger by `user_id` and `project_id`, and `TouchProjects` is held to the
//! same rule although it reads no field today — see
//! [`crate::sql::scope_of`].
//!
//! **`request_id` ARRIVES AS A HEADER WHEN THERE IS NO `Scope` TO CARRY IT.**
//! `project`'s `ListProjects` and `ResolveProject` carry no `Scope`, so
//! `project` sends none and forwards D67's id as `x-yadgar-request-id`
//! (ledger 1254). Every handler reads that header and
//! [`crate::sql::tel_scope`] records it when no `Scope` carries an id.
//!
//! # This module is in ADR-0522's PRE-ENFORCEMENT state, deliberately
//!
//! `yadgar/common/v1`'s `Scope.owner_reads_own_record` admits exactly two states
//! and names the discriminator — "WHICH ONE IT IS IN IS DECIDED BY WHETHER IT
//! READS THIS FIELD" — and a `-db` that reads it must refuse every call carrying
//! an unset one. **Nothing in this crate reads it.** The setting widens a
//! visibility ladder so an owner outside the team of their own record can still
//! read it; there is an owner here, but no ladder and no team axis for the
//! setting to widen, so there is nothing for it to widen. Reading it would make this service refuse every read
//! in the estate until a gateway populated a policy that decides nothing.
//!
//! Stated here rather than left as an absence, because copying a sibling
//! faithfully is exactly how the field would arrive.

use sqlx::MySqlPool;
use tonic::{Request, Response, Status};
use yadgar_lifecycle::serve_tls::caller_common_name;
use yadgar_telemetry::estimator::Class;
use yadgar_telemetry::grpc::status_name;
use yadgar_telemetry::observe::{Call, Outcome};
use yadgar_telemetry::pb::yadgar::telemetry::v1::Kind;

use crate::pb::yadgar::project::v1::project_db_service_server::ProjectDbService;
use crate::pb::yadgar::project::v1::*;
use crate::sql::{request_id_of, tel_scope};

/// This service's name, on every telemetry record and on the rotation watcher's
/// gauges. ONE spelling, because a dashboard selects on it.
pub const SERVICE: &str = "project-db";

/// The ONE caller `SyncProjects` admits: the common name on `project`'s client
/// leaf, as `platform` issues it (`project-client-tls`, ADR-0936).
pub const SYNC_CALLER: &str = "project-caller";

/// The ONE caller `RegisterProject` admits: the same leaf as [`SYNC_CALLER`]
/// (ADR-1007). Stated as its own name because the two gates are two rulings,
/// and a change to one is not a change to the other.
pub const REGISTER_CALLER: &str = "project-caller";

pub struct ProjectDb {
    pub(crate) pool: MySqlPool,
}

impl ProjectDb {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }
}

/// What every handler records. `rows` is the one field that differs, and it is
/// the one a blanket value gets wrong: the row count for a list is the LIST, not
/// one, or every page looks like a single-row read.
fn envelope<T: prost::Message + Rendered>(response: &T, rows: u32) -> Outcome {
    Outcome {
        status: "OK",
        payload: response.rendered(),
        encoded_bytes: Some(prost::Message::encoded_len(response) as u64),
        class: Class::Envelope,
        rows,
        ..Default::default()
    }
}

/// The text the telemetry crate measures for a response's WORD count: its
/// paths, ids and counts, and never a user id (yaadgaar #167).
///
/// **NOT `{response:?}`.** `Meta` carries three user ids — `owner_user_id`,
/// `created_by` and `updated_by` — and a Debug rendering of any response that
/// holds a `Meta` put all three in the payload. The telemetry crate uses the
/// payload only to count words (bytes come from `encoded_bytes`), so no owner
/// id from a response reached a record through the payload; but the string is
/// the one this service hands away, and an id that never enters it cannot leak
/// through a later telemetry change.
///
/// **AN ALLOWLIST, ONE IMPL PER RESPONSE.** A field added to a response later
/// is not rendered until someone writes it here, so a new user id field fails
/// closed rather than arriving in the payload by default.
trait Rendered {
    fn rendered(&self) -> String;
}

/// A `Meta` as the payload carries it: the row's id and its path.
fn meta_words(meta: Option<&crate::pb::yadgar::common::v1::Meta>) -> String {
    meta.map_or_else(String::new, |m| format!("{} {}", m.id, m.project_id))
}

/// A `Project` as the payload carries it: its id and its path.
fn project_words(project: &Project) -> String {
    let id = project.meta.as_ref().map_or("", |m| m.id.as_str());
    format!("{id} {}", project.path)
}

impl Rendered for RegisterProjectResponse {
    fn rendered(&self) -> String {
        meta_words(self.meta.as_ref())
    }
}

impl Rendered for RenameProjectResponse {
    fn rendered(&self) -> String {
        meta_words(self.meta.as_ref())
    }
}

impl Rendered for ArchiveProjectResponse {
    fn rendered(&self) -> String {
        meta_words(self.meta.as_ref())
    }
}

impl Rendered for GetProjectResponse {
    fn rendered(&self) -> String {
        self.project
            .as_ref()
            .map_or_else(String::new, project_words)
    }
}

impl Rendered for ListProjectsResponse {
    fn rendered(&self) -> String {
        self.projects
            .iter()
            .map(project_words)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Rendered for ResolveProjectResponse {
    fn rendered(&self) -> String {
        format!(
            "{} exact={} via_alias={} status={} {}",
            self.resolved_path, self.exact, self.via_alias, self.status, self.source_repo
        )
    }
}

impl Rendered for SyncProjectsResponse {
    fn rendered(&self) -> String {
        format!(
            "inserted={} updated={} archived={} reactivated={} unchanged={}",
            self.inserted, self.updated, self.archived, self.reactivated, self.unchanged
        )
    }
}

impl Rendered for TouchProjectsResponse {
    /// Empty on the wire; one word, as its Debug rendering was, so the record
    /// keeps a token estimate rather than losing the field.
    fn rendered(&self) -> String {
        "TouchProjectsResponse".to_string()
    }
}

#[tonic::async_trait]
impl ProjectDbService for ProjectDb {
    async fn register_project(
        &self,
        request: Request<RegisterProjectRequest>,
    ) -> Result<Response<RegisterProjectResponse>, Status> {
        // READ BEFORE THE REQUEST IS CONSUMED, as in `sync_projects`: the peer
        // chain lives on the request's extensions, and `into_inner` drops them.
        let caller = caller_common_name(&request);
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "RegisterProject",
            Kind::Write,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.register_as(caller.as_deref(), req).await },
            |r| envelope(r, 1),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn resolve_project(
        &self,
        request: Request<ResolveProjectRequest>,
    ) -> Result<Response<ResolveProjectResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "ResolveProject",
            Kind::Read,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.resolve(req).await },
            |r| envelope(r, 1),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn get_project(
        &self,
        request: Request<GetProjectRequest>,
    ) -> Result<Response<GetProjectResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "GetProject",
            Kind::Read,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.get(req).await },
            |r| envelope(r, 1),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn list_projects(
        &self,
        request: Request<ListProjectsRequest>,
    ) -> Result<Response<ListProjectsResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "ListProjects",
            Kind::Read,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.list(req).await },
            |r| envelope(r, r.projects.len() as u32),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn touch_projects(
        &self,
        request: Request<TouchProjectsRequest>,
    ) -> Result<Response<TouchProjectsResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "TouchProjects",
            Kind::Write,
            tel_scope(&req.scope, request_id),
        );
        // THE ROW COUNT IS THE SIZE OF THE FLUSH, and it is read before the
        // request is consumed. `TouchProjectsResponse` is empty, so a count taken
        // from the response would be one for a statement that wrote five hundred
        // rows — and this rpc's whole cost profile is that number.
        let rows = req.paths.len() as u32;

        call.run(
            async move { self.touch(req).await },
            |r| envelope(r, rows),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn sync_projects(
        &self,
        request: Request<SyncProjectsRequest>,
    ) -> Result<Response<SyncProjectsResponse>, Status> {
        // READ BEFORE THE REQUEST IS CONSUMED: the peer chain lives on the
        // request's extensions, and `into_inner` drops them.
        let caller = caller_common_name(&request);
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "SyncProjects",
            Kind::Write,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.sync_as(caller.as_deref(), req).await },
            |r| {
                envelope(
                    r,
                    r.inserted + r.updated + r.archived + r.reactivated + r.unchanged,
                )
            },
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn rename_project(
        &self,
        request: Request<RenameProjectRequest>,
    ) -> Result<Response<RenameProjectResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "RenameProject",
            Kind::Write,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.rename(req).await },
            |r| envelope(r, 1),
            status_name,
        )
        .await
        .map(Response::new)
    }

    async fn archive_project(
        &self,
        request: Request<ArchiveProjectRequest>,
    ) -> Result<Response<ArchiveProjectResponse>, Status> {
        let request_id = request_id_of(&request);
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "ArchiveProject",
            Kind::Write,
            tel_scope(&req.scope, request_id),
        );

        call.run(
            async move { self.archive(req).await },
            |r| envelope(r, 1),
            status_name,
        )
        .await
        .map(Response::new)
    }
}

#[cfg(test)]
mod tests;
