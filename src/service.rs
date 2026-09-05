//! `ProjectDbService`: the RPC surface, its instrumentation, and the two things
//! this module deliberately does NOT do with a `Scope`.
//!
//! One RPC is one transaction (D5). The operations themselves live next door —
//! `read` for the three that answer questions, `write` for the four that change
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
//! subtree filter, that field would be redundant on the one rpc that has it. And
//! `Project` carries no `visibility`, no `team_id` and no owner — the three
//! fields D12's ladder is built from — so there is nothing on the entity for a
//! per-row decision to be made against.
//!
//! **SO THE REGISTRY IS READABLE DEPLOYMENT-WIDE, AND THAT IS STATED RATHER THAN
//! LEFT TO BE INFERRED FROM CODE THAT HAPPENS NOT TO FILTER.** One deployment is
//! one organisation (D27), and what this store holds is the set of names that
//! organisation has registered — not the content filed under them. A caller who
//! can reach this service can list every project path in the installation. If
//! that is ever wrong, it is wrong at the level of who may reach the service, and
//! the change is a contract change rather than a predicate quietly added here.
//!
//! `Scope` is still REQUIRED on every rpc and refused when absent: it carries
//! `request_id`, without which D67's records cannot be summed across hops, and
//! `user_id` and `project_id`, which key D9's idempotency ledger. An absent one
//! is a programming error upstream.
//!
//! # This module is in ADR-0522's PRE-ENFORCEMENT state, deliberately
//!
//! `yadgar/common/v1`'s `Scope.owner_reads_own_record` admits exactly two states
//! and names the discriminator — "WHICH ONE IT IS IN IS DECIDED BY WHETHER IT
//! READS THIS FIELD" — and a `-db` that reads it must refuse every call carrying
//! an unset one. **Nothing in this crate reads it.** The setting widens a
//! visibility ladder so an owner outside the team of their own record can still
//! read it; there is no ladder here, no team axis, and no owner, so there is
//! nothing for it to widen. Reading it would make this service refuse every read
//! in the estate until a gateway populated a policy that decides nothing.
//!
//! Stated here rather than left as an absence, because copying a sibling
//! faithfully is exactly how the field would arrive.

use sqlx::MySqlPool;
use tonic::{Request, Response, Status};
use yadgar_telemetry::estimator::Class;
use yadgar_telemetry::grpc::status_name;
use yadgar_telemetry::observe::{Call, Outcome};
use yadgar_telemetry::pb::yadgar::telemetry::v1::Kind;

use crate::pb::yadgar::project::v1::project_db_service_server::ProjectDbService;
use crate::pb::yadgar::project::v1::*;
use crate::sql::tel_scope;

/// This service's name, on every telemetry record and on the rotation watcher's
/// gauges. ONE spelling, because a dashboard selects on it.
pub const SERVICE: &str = "project-db";

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
fn envelope<T: prost::Message + std::fmt::Debug>(response: &T, rows: u32) -> Outcome {
    Outcome {
        status: "OK",
        payload: format!("{response:?}"),
        encoded_bytes: Some(prost::Message::encoded_len(response) as u64),
        class: Class::Envelope,
        rows,
        ..Default::default()
    }
}

#[tonic::async_trait]
impl ProjectDbService for ProjectDb {
    async fn register_project(
        &self,
        request: Request<RegisterProjectRequest>,
    ) -> Result<Response<RegisterProjectResponse>, Status> {
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "RegisterProject",
            Kind::Write,
            tel_scope(&req.scope),
        );

        call.run(
            async move { self.register(req).await },
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
        let req = request.into_inner();
        let call = Call::start(SERVICE, "ResolveProject", Kind::Read, tel_scope(&req.scope));

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
        let req = request.into_inner();
        let call = Call::start(SERVICE, "GetProject", Kind::Read, tel_scope(&req.scope));

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
        let req = request.into_inner();
        let call = Call::start(SERVICE, "ListProjects", Kind::Read, tel_scope(&req.scope));

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
        let req = request.into_inner();
        let call = Call::start(SERVICE, "TouchProjects", Kind::Write, tel_scope(&req.scope));
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

    async fn rename_project(
        &self,
        request: Request<RenameProjectRequest>,
    ) -> Result<Response<RenameProjectResponse>, Status> {
        let req = request.into_inner();
        let call = Call::start(SERVICE, "RenameProject", Kind::Write, tel_scope(&req.scope));

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
        let req = request.into_inner();
        let call = Call::start(
            SERVICE,
            "ArchiveProject",
            Kind::Write,
            tel_scope(&req.scope),
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
