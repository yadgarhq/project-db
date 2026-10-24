//! `SyncProjects`, `RegisterProject` and `TouchProjects` — and the two verbs
//! this release REFUSES in as many words.
//!
//! One RPC is one transaction (D5), and the D9 claim is taken inside it, so a
//! write and the record that it happened commit together or not at all.
//!
//! # `RenameProject` and `ArchiveProject` answer `UNIMPLEMENTED`
//!
//! **RENAME IS HELD BACK ON A DATA ARGUMENT RATHER THAN AN EFFORT ONE.** A
//! rename is an alias and never a rewrite (D53), which keeps every stored
//! `Meta.project_id` VALID — but it does not keep it CURRENT. Every memory, wiki
//! page, ADR and task already stamped with the old path goes on carrying it, and
//! the job that would retag them does not exist. So a rename shipped today
//! silently orphans records instead of moving them: they resolve, they are
//! readable, and no view of the new path contains them.
//!
//! **THAT ARGUMENT DOES NOT COVER A STATUS-ONLY ARCHIVE, AND THIS HEADER USED TO
//! SAY IT DID.** An archive that only sets `status = ARCHIVED` moves no record
//! and changes no resolution: neither resolver consults status, so a record
//! stamped with the path still resolves `exact` to the archived row
//! (`plans/the-d35-seeder.md`, "Archive is a status flip"). `SyncProjects`
//! archives exactly that way. The per-row `ArchiveProject` rpc stays
//! `UNIMPLEMENTED` for a different reason: ADR-0705 builds no per-row Register,
//! Rename or Archive verb for organisation projects, and the whole-set sync is
//! the one writer of them (ADR-0936).
//!
//! **REFUSED, NEVER OMITTED.** A gRPC service that silently lacks a contracted
//! method answers `UNIMPLEMENTED` from tonic's own fallback, with no reason in
//! it, and a caller cannot tell that from a version skew or a bad route. These
//! refuse with the reason written out, which is the estate's existing shape —
//! `SetInheritedSetting` sat `UNIMPLEMENTED` with a stated reason for the same
//! kind of ordering constraint. Both come back with the retag job, together.
//!
//! **THE REFUSAL IS THE FIRST THING EITHER HANDLER DOES**, before any
//! transaction — so neither ever ATTEMPTS a claim, and there is no D9 row to
//! reason about. That is what makes an idempotency key offered to a held-back
//! verb still free afterwards.
//!
//! It is not the ORDER that buys it, and this file used to say it was. The claim
//! is written inside the caller's transaction (`src/idem.rs`), so a refusal
//! BELOW it rolls the claim back with everything else and spends no key either —
//! `register` refuses that way twice, twenty lines apart, and
//! `tests/idempotency.rs::a_failed_write_leaves_no_claim_behind` is the proof.
//! The invariant is guaranteed by transaction atomicity. Refusing early is a
//! COST argument: a request that cannot succeed should not take a connection out
//! of the pool to be told so.
//!
//! # Two writers, disjoint by class
//!
//! Organisation-level projects are defined by GitOps rather than created on
//! demand (D43, D52): `SyncProjects` (`write/sync.rs`) is their ONE writer,
//! callable by `project` only and relayed from the D35 seeder (ADR-0934,
//! ADR-0936). `RegisterProject` is the PRIVATE-class path (`local/...`, ledger
//! 641) and REFUSES an org-class path with `FAILED_PRECONDITION`, so the two
//! writers are disjoint by `ck_project_class`, the engine's own constraint —
//! the one-writer rule is mechanical rather than a convention. Nothing in the
//! estate calls `RegisterProject` yet; ledger 641 is its caller, and that
//! caller is `project`: the verb admits `project`'s leaf only, as
//! `SyncProjects` does (ADR-1007).
//!
//! # The one namespace rule that spans two tables
//!
//! An alias and a live path share one namespace and no single constraint can say
//! so: two tables cannot hold one unique index between them. So the rule lives
//! here — nothing registers at a path that is a former path of something else.

use tonic::Status;

use crate::pb::yadgar::project::v1::*;
use crate::service::ProjectDb;

mod grants;
mod register;
mod sync;
mod touch;

/// Why `RenameProject` is refused — see this module's header.
const HELD_BACK: &str = "this release does not implement it, and the reason is the records rather \
     than the code. Every memory, wiki page, ADR and task already stamped with a project path goes \
     on carrying it, and the job that retags them does not exist yet — so moving a project today \
     would leave those records readable, valid, and absent from every view of the project they \
     belong to. RenameProject returns with that retag job.";

/// Why `ArchiveProject` is refused — see this module's header.
const NO_PER_ROW_ARCHIVE: &str = "organisation projects are declared in the organisation's GitOps \
     repository and written by the D35 seeder through SyncProjects, which archives a project the \
     declaration stops naming (ADR-0705, ADR-0934, ADR-0936). There is no per-row archive verb.";

impl ProjectDb {
    /// Held back until records can be retagged — see this module's header.
    ///
    /// The request is not validated first and no transaction is opened. There is
    /// nothing to validate a request AGAINST when no request of this shape can
    /// succeed, and an `INVALID_ARGUMENT` here would tell a caller to correct a
    /// value that would then be refused anyway.
    pub(crate) async fn rename(
        &self,
        _req: RenameProjectRequest,
    ) -> Result<RenameProjectResponse, Status> {
        Err(Status::unimplemented(format!("RenameProject: {HELD_BACK}")))
    }

    /// Not a verb this module builds — see this module's header.
    pub(crate) async fn archive(
        &self,
        _req: ArchiveProjectRequest,
    ) -> Result<ArchiveProjectResponse, Status> {
        Err(Status::unimplemented(format!(
            "ArchiveProject: {NO_PER_ROW_ARCHIVE}"
        )))
    }
}
