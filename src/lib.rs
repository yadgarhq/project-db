//! `project-db` — the only writer of the project registry (D4, D52).
//!
//! It holds no business rules. Its job is the boundary: one call is one
//! transaction (D5), and the logic service reaches this store only over gRPC —
//! never by opening a connection of its own.
//!
//! **WHAT THIS MODULE IS FOR.** Every module in the estate scopes its rows on a
//! project id, and until this exists nothing validates that the id names a real
//! project. A typo silently mints a phantom scope and the symptom is "my tasks
//! vanished" (D52). The registry is a BASE REQUIREMENT rather than a feature of
//! `task`: `ask`, `recall` and the audit store each need the same key, which is
//! why it is its own twin instead of a check inside a module that owns
//! something else (D7, D54).
//!
//! **THE THREE PROPERTIES THE CONTRACT'S OWN COMMENTS OBLIGE, in one place:**
//!
//! - `Project.path` is the canonical hierarchical id (D53), and it is the value
//!   that lands in every other entity's `Meta.project_id`. The hierarchy is
//!   SEMANTIC, so resolution walks it segment by segment — see `crate::path`.
//! - A rename is an ALIAS, never a rewrite of every record carrying the old path
//!   (D53). `ResolveProject` and `GetProject` both follow one — and the store
//!   holds aliases, which is why those read paths exist before the verb that
//!   creates one does.
//! - `last_seen_at` is DEBOUNCED, not written per request (D52). Its only
//!   consumer is D47's notice expiry, which reasons in days.
//!
//! **TWO CONTRACTED VERBS ANSWER `UNIMPLEMENTED` IN THIS RELEASE.**
//! `RenameProject` and `ArchiveProject` refuse with the reason written out: an
//! alias keeps a stored `Meta.project_id` VALID but does not keep it CURRENT,
//! and the job that retags the records already carrying an old path does not
//! exist — so either verb shipped today would leave those records readable and
//! absent from every view of the project they belong to. `src/write.rs` carries
//! the whole argument. `RegisterProject` is implemented and has no caller:
//! organisation-level projects are defined by GitOps rather than created on
//! demand (D43).
//!
//! **ADR-0227: an identity is never derived.** A caller supplies a path or the
//! call is refused. Nothing here trims, lowercases, normalises or invents one,
//! and nothing is ever auto-created.

#![forbid(unsafe_code)]

pub mod boot;
mod idem;
pub mod path;
mod read;
pub mod rotate;
mod rows;
pub mod schema;
pub mod service;
mod sql;
mod write;

/// Generated from the vendored contract (D16, D70).
///
/// The module tree MIRRORS the protobuf package path, and has to: generated
/// cross-package references are emitted as `super::super::common::v1::Meta`, so
/// a flattened tree fails to compile with an error that points at generated code
/// rather than at this file.
pub mod pb {
    pub mod yadgar {
        pub mod common {
            pub mod v1 {
                tonic::include_proto!("yadgar.common.v1");
            }
        }
        pub mod project {
            pub mod v1 {
                tonic::include_proto!("yadgar.project.v1");
            }
        }
    }
}
