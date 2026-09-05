//! This module's migrations. `yadgar-store` runs them; it never holds them (D7).
//!
//! Migrations are APPENDED, never edited. Every one of these will have run
//! against a live database, and a store that has applied version 1 will never
//! apply it again — so a correction to an old migration is a correction only new
//! installations receive, which is the worst of both.
//!
//! One function each rather than one list of literals: a migration is an
//! independent, immutable unit, and giving each a name puts its reasoning next
//! to its SQL instead of in a comment halfway down a table.

use yadgar_store::migrate::{Migration, MigrationError, MigrationSet};

pub fn migrations() -> Result<MigrationSet, MigrationError> {
    MigrationSet::new(all())
}

/// One list, in one place, so the set is stated once.
fn all() -> Vec<Migration> {
    vec![
        create_project(),
        project_alias(),
        project_write_idempotency(),
    ]
}

/// `id` is a URN carrying a UUIDv7 (D42) — never the engine's integer key, which
/// stops being portable the moment a module swaps engines (D7).
///
/// `path` is the CANONICAL hierarchical id (D53) and the value that lands in
/// every other entity's `Meta.project_id`. It is UNIQUE: two rows claiming one
/// path is the split-corpus failure this whole module exists to prevent, and a
/// constraint is the only place that claim survives a concurrent writer.
///
/// **`updated_at` CARRIES NO `ON UPDATE CURRENT_TIMESTAMP`, WHICH IS THE ONE
/// PLACE THIS TABLE DEPARTS FROM ITS SIBLING'S SHAPE.** `task` has no write that
/// is not a change; this table does. `TouchProjects` is a debounced bookkeeping
/// flush (D52) that writes `last_seen_at` and nothing else, and it may run
/// hundreds of times between two registrations. An `ON UPDATE` clause would make
/// every one of those look like an edit of the registration — so `updated_at`
/// would answer "when was this project last touched by anything", which is what
/// `last_seen_at` is FOR, and the field that meant "when did this registration
/// last change" would no longer exist. The three mutating RPCs set it
/// explicitly instead. Same argument, one field over, is why `TouchProjects`
/// does not increment `version`: that is D8's compare-and-set counter, and a
/// background write moving it would fail every concurrent `RenameProject` for a
/// reason no caller could see.
///
/// `last_seen_at` is NULL until something is seen. Absent and "seen at the
/// epoch" are different facts and only one of them is true of a project that
/// nothing has run under yet.
///
/// **`status` HAS ONE WRITER IN THIS RELEASE AND IT ONLY EVER WRITES `ACTIVE`,
/// AND `ix_project_status` IS INDEXED ANYWAY** — the same shape as `project_alias`
/// below, and stated so the two are not read as one deliberate choice and one
/// oversight. `ArchiveProject` is held back until records can be retagged
/// (`src/write.rs`), so nothing in-process produces `ARCHIVED`; but
/// `ListProjects`'s status filter and the `status` `ResolveProject` reports are
/// contract obligations TODAY, so both are implemented and both are tested
/// against a seeded row. The column and its index belong to the store rather
/// than to the verb, which is what makes that verb's return an edit to
/// `write.rs` alone rather than a migration on a live database.
///
/// **NOTHING MOVES `version` EITHER, FOR THE SAME REASON.** `RenameProject` and
/// `ArchiveProject` were its only movers, so every row in this release sits at 1
/// and D8's compare-and-set has no writer to protect against yet. It is still
/// the counter the contract declares and the value `Meta.version` carries, and
/// `TouchProjects` is held to not moving it — see that function — precisely so
/// the guarantee is already true when the movers arrive.
fn create_project() -> Migration {
    Migration {
        version: 1,
        name: "create_project".into(),
        sql: "CREATE TABLE project (
                  id            VARCHAR(96)     NOT NULL PRIMARY KEY,
                  version       BIGINT UNSIGNED NOT NULL DEFAULT 1,
                  path          VARCHAR(255)    NOT NULL,
                  display_name  VARCHAR(255)    NOT NULL,
                  status        TINYINT         NOT NULL,
                  created_by    VARCHAR(64)     NOT NULL,
                  updated_by    VARCHAR(64)     NOT NULL,
                  created_at    TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  updated_at    TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  last_seen_at  TIMESTAMP       NULL     DEFAULT NULL,
                  UNIQUE KEY uq_project_path (path),
                  KEY ix_project_status (status)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            .into(),
    }
}

/// A rename is an ALIAS, never a rewrite of every record carrying the old path
/// (D53). This is the table that makes that possible.
///
/// **THE ALIAS IS THE PRIMARY KEY, and that is the constraint doing the work.**
/// One former path resolves to exactly one project, enforced by the engine
/// rather than by a check some later code path forgets. Without it a path
/// renamed away from two projects in turn resolves to whichever row a scan
/// happened to reach first, which is a coin-flip nobody would ever see.
///
/// It is a separate table rather than a JSON column on `project` because the
/// LOOKUP is the point: `ResolveProject` reads by alias on the request path, so
/// the alias needs to be a key. A JSON array would make every resolution a scan
/// of every project.
///
/// **NOTHING WRITES THIS TABLE IN THIS RELEASE, AND IT IS CREATED ANYWAY.**
/// `RenameProject` is held back until records can be retagged (`src/write.rs`),
/// so no rpc mints an alias today — but every READ path already follows one,
/// because `Project.aliases` and `ResolveProject.via_alias` are contract
/// obligations now rather than later. Creating the table with the store rather
/// than with the verb also keeps the two apart: the verb's return is then a
/// change to `write.rs` alone, not a migration on a live database.
///
/// **AN ALIAS AND A LIVE PATH SHARE ONE NAMESPACE, and no single constraint can
/// say so.** Two tables cannot hold one unique index between them. The rule —
/// that a path is never simultaneously a live path and an alias — is therefore
/// enforced in `write.rs`, inside the transaction that would break it, and
/// `tests/registry.rs` is what keeps it true.
fn project_alias() -> Migration {
    Migration {
        version: 2,
        name: "project_alias".into(),
        sql: "CREATE TABLE project_alias (
                  alias_path VARCHAR(255) NOT NULL PRIMARY KEY,
                  project_id VARCHAR(96)  NOT NULL,
                  created_at TIMESTAMP    NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  KEY ix_project_alias_project (project_id)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            .into(),
    }
}

/// D9: a repeated key is a replay, so the ORIGINAL outcome has to still exist to
/// be returned. `response` is the encoded response message and `rpc` names which
/// one it is — decoding a stored `RegisterProjectResponse` as an
/// `ArchiveProjectResponse` would succeed and mean nothing.
///
/// Keyed on (project, user, key) rather than on the key alone: the key is
/// CLIENT-supplied, so two clients will eventually choose the same string, and
/// deduplicating across users would hand one of them the other's record.
///
/// **`request_fingerprint` IS `NOT NULL` HERE, WHERE `task-db`'s IS NULLABLE,
/// AND THE DIFFERENCE IS NOT A DISAGREEMENT.** That column is nullable there
/// because it was ADDED to a table already holding rows, and NULL is the only
/// value meaning "there is nothing to compare against" — an absent digest must
/// replay, or the migration implementing D9's amendment would regress D9's core
/// rule. This table has the column from its first migration, so no row can lack
/// one and no NULL is reachable. Declaring it nullable would add a branch to
/// `replay` that nothing can enter and no test can prove.
fn project_write_idempotency() -> Migration {
    Migration {
        version: 3,
        name: "project_write_idempotency".into(),
        sql: "CREATE TABLE project_write (
                  project_id          VARCHAR(255)    NOT NULL,
                  user_id             VARCHAR(64)     NOT NULL,
                  idem_key            VARCHAR(255)    NOT NULL,
                  rpc                 VARCHAR(32)     NOT NULL,
                  response            VARBINARY(4096) NOT NULL,
                  request_fingerprint BINARY(32)      NOT NULL,
                  created_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  PRIMARY KEY (project_id, user_id, idem_key)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
            .into(),
    }
}
