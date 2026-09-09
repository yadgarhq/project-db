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
        pin_the_path_collation(),
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

/// **THE TWO PATH COLUMNS DECIDE WHETHER A PROJECT PATH IS CASE-SENSITIVE, AND
/// UNTIL NOW NOTHING IN THIS REPOSITORY SAID SO.** Migrations 1 and 2 declare
/// `DEFAULT CHARSET=utf8mb4` with no `COLLATE`, so `project.path` and
/// `project_alias.alias_path` take whatever `@@collation_server` happens to be.
/// This migration writes the answer down.
///
/// **THE CODE HAS AN OPINION AND THE COLUMN DID NOT.** Two call sites fold ASCII
/// case in Rust because they believe the engine does: `path::refuse_reserved_root`,
/// so `LOCAL` cannot occupy the slot reserved for `local`, and
/// `write::touch::deduplicated`, so two spellings of one path are not counted as two
/// paths resolving to one row. Both are right today and neither is guaranteed.
/// Measured on `mariadb:11.8.9` against the tables exactly as migrations 1 and 2
/// declare them, `information_schema.COLUMNS` reports `utf8mb4_uca1400_ai_ci`
/// for both columns — case-insensitive, because that is this image's server
/// default. An operator whose engine defaults to `utf8mb4_bin` or to a `_cs`
/// collation gets a store in which `alpha` and `ALPHA` are two rows; then
/// `deduplicated` folds them into one path, touches one row, reports that
/// everything it named resolved, and silently never advances the other project's
/// `last_seen_at`. Nothing anywhere reports that. It is D80's second failure mode
/// exactly: a behaviour whose correctness rests on an environment default nobody
/// declared.
///
/// **`utf8mb4_general_ci` RATHER THAN THE NAME THE COLUMN CARRIES TODAY, and
/// rather than `utf8mb4_bin`.** Within the alphabet `path::validate` admits —
/// `[A-Za-z0-9._-]` — the choice among case-insensitive collations is
/// portability and not semantics, and that is measured rather than assumed. On
/// `mariadb:11.8.9`, for `utf8mb4_uca1400_ai_ci`, `utf8mb4_general_ci` and
/// `utf8mb4_unicode_ci` alike: `alpha` = `ALPHA` and `local` = `LOCAL` hold,
/// while `lo-cal`, `l.ocal` and `lo_cal` are each UNEQUAL to `local` and to each
/// other. ASCII case is the whole of what any of them folds here. So this pins
/// the behaviour the code already assumes, and it pins it under a name every
/// engine knows: `utf8mb4_uca1400_ai_ci` exists only on MariaDB 11.4 and later,
/// so naming it would fail this migration on any older engine for a difference no
/// path can express.
///
/// **`utf8mb4_bin` IS THE OTHER COHERENT ANSWER AND IT IS NOT THIS ONE.** It
/// would make paths case-SENSITIVE, which inverts both decisions above:
/// `refuse_reserved_root` would be turning away `LOCAL`, a name the store would
/// then hold happily as an ordinary organisation, and `deduplicated` would have
/// to stop folding or lose a project's flush. That is a decision about what a
/// project path IS, it reaches every `Meta.project_id` in the estate, and it
/// belongs in the record rather than in a migration. Pinned first, argued
/// separately: whichever way that goes, it should not also be the moment the
/// column stops depending on a server setting.
///
/// **WHAT IT COSTS.** A collation change on an indexed column is a table
/// rebuild: MariaDB copies `project` and `project_alias` and rebuilds
/// `uq_project_path` and the alias primary key under a metadata lock, so writes
/// to those two tables wait for the duration. Measured on `mariadb:11.8.9`
/// against populated copies of both tables — four projects, two aliases, mixed
/// case — both statements succeed and every row survives with its bytes intact:
/// `acme/Forecast-2` and `ACME/Older` come back spelled as they went in.
///
/// **IT CAN FAIL, IN EXACTLY ONE CASE, AND THAT FAILURE IS THE CORRECT ONE.**
/// From any case-INSENSITIVE starting collation it cannot: `general_ci` and
/// `uca1400_ai_ci` agree over the whole admitted alphabet, so no two rows
/// distinct before are equal after. From `utf8mb4_bin` it can, and measured it
/// does — with `acme/forecast` and `ACME/FORECAST` both present, the `ALTER`
/// answers `ERROR 1062 Duplicate entry for key 'uq_project_path'`. A store in
/// that state is already holding the split-corpus condition this module exists
/// to prevent, and every case-folding call site has been reading it wrongly for
/// as long as it has existed. Refusing loudly is better than pinning a collation
/// over it. An operator meeting 1062 has two rows to reconcile before this
/// applies, and the message names the index that says which.
///
/// Today none of that arises: this module has no tag, no `yadgar-deployable`
/// topic and no `argocd/versions` entry, so there is no populated column
/// anywhere to rebuild.
///
/// **THE TWO PATH COLUMNS MOVE TOGETHER AND NOTHING ELSE MOVES WITH THEM.**
/// `project.display_name` and `project_write.project_id` keep the server
/// default, so this table now carries mixed collations on purpose. The rule is
/// that a column moves when something COMPARES it against another path — `path`
/// and `alias_path` are joined in `read.rs` and in `touch`'s `matches`, and a
/// mismatch between two compared columns is `ERROR 1267 Illegal mix of
/// collations`. Nothing compares `display_name` or `project_id` to either, so
/// neither is pinned here; a later column that IS compared to a path belongs in
/// this migration's company rather than on the default.
///
/// **APPENDED RATHER THAN EDITED INTO MIGRATION 1**, which is this file's
/// standing rule. The rule's usual reason — that migration 1 has already run
/// somewhere — happens to be false for this module today, and following it
/// anyway is what keeps a developer's existing database and a fresh one the same
/// schema.
///
/// The two statements are one migration because they are one decision. DDL is
/// not transactional on this engine, so a failure between them leaves the ledger
/// row unwritten and the next boot re-runs both — and re-applying a collation a
/// column already carries is a no-op.
fn pin_the_path_collation() -> Migration {
    Migration {
        version: 4,
        name: "pin_the_path_collation".into(),
        sql: "ALTER TABLE project
                MODIFY path VARCHAR(255)
                  CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci NOT NULL;
              ALTER TABLE project_alias
                MODIFY alias_path VARCHAR(255)
                  CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci NOT NULL"
            .into(),
    }
}
