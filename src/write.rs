//! `RegisterProject` and `TouchProjects` — and the two verbs this release
//! REFUSES in as many words.
//!
//! One RPC is one transaction (D5), and the D9 claim is taken inside it, so a
//! write and the record that it happened commit together or not at all.
//!
//! # `RenameProject` and `ArchiveProject` answer `UNIMPLEMENTED`
//!
//! **BOTH ARE HELD BACK ON A DATA ARGUMENT RATHER THAN AN EFFORT ONE.** A rename
//! is an alias and never a rewrite (D53), which keeps every stored
//! `Meta.project_id` VALID — but it does not keep it CURRENT. Every memory, wiki
//! page, ADR and task already stamped with the old path goes on carrying it, and
//! the job that would retag them does not exist. So a rename shipped today
//! silently orphans records instead of moving them: they resolve, they are
//! readable, and no view of the new path contains them. Archiving has the same
//! shape one step earlier — an instance must be able to retag before anything is
//! archived, or the archive is the moment the records stop being findable.
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
//! # `RegisterProject` has no caller in this release
//!
//! Organisation-level projects are defined by GitOps rather than created on
//! demand (D43, D52), so this rpc is not the creation path for them. It is
//! implemented, tested and served because the contract declares it and because
//! the store has to be able to hold a registration at all — but nothing in the
//! estate calls it yet, and that is deliberate rather than an oversight.
//!
//! # The one namespace rule that spans two tables
//!
//! An alias and a live path share one namespace and no single constraint can say
//! so: two tables cannot hold one unique index between them. So the rule lives
//! here — nothing registers at a path that is a former path of something else.

use tonic::Status;

use crate::pb::yadgar::project::v1::*;
use crate::service::ProjectDb;

mod register;
mod touch;

/// What both held-back verbs say, in one place so the two cannot drift into
/// giving different reasons for one decision.
const HELD_BACK: &str = "this release does not implement it, and the reason is the records rather \
     than the code. Every memory, wiki page, ADR and task already stamped with a project path goes \
     on carrying it, and the job that retags them does not exist yet — so moving or archiving a \
     project today would leave those records readable, valid, and absent from every view of the \
     project they belong to. RenameProject and ArchiveProject return together with that retag job.";

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

    /// Held back until records can be retagged — see this module's header.
    pub(crate) async fn archive(
        &self,
        _req: ArchiveProjectRequest,
    ) -> Result<ArchiveProjectResponse, Status> {
        Err(Status::unimplemented(format!(
            "ArchiveProject: {HELD_BACK}"
        )))
    }
}
