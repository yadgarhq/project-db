//! `SyncProjects` — the organisation's whole declaration, applied in ONE
//! transaction (D5), and the only writer of org-class rows (ADR-0936).
//!
//! # The order, and what each step buys
//!
//! 1. **The caller.** `project`'s leaf only (`project-caller`), checked before
//!    anything else — so a refused caller never takes a connection out of the
//!    pool. mTLS proves "one of ours"; this proves "which one".
//! 2. **The whole request, validated.** Any refusal names the path and writes
//!    nothing, because nothing has been opened yet.
//! 3. **The sentinel lock**, the first statement in the transaction (migration
//!    5, `crate::schema::project_sync_lock`). Two syncs serialise on it, two
//!    first syncs into an empty store included.
//! 4. **The read.** Every org-class row, AFTER the lock. A plain `BEGIN` takes
//!    no snapshot until the first consistent read, and the first statement is
//!    the locking read above — so this read sees every sync that committed
//!    before the lock was granted, whatever the session's isolation level.
//! 5. **The alias rule**, the one `register` applies: nothing lands at a path
//!    that is a former path of something else.
//! 6. **The mass-archive guard** (ADR-0939), decided before a single write.
//! 7. **The writes**, then the commit.
//!
//! # Case
//!
//! `path` collates `utf8mb4_general_ci` (migration 4), so `acme/Alpha` and
//! `acme/alpha` are one row, and every comparison here folds ASCII case to
//! match. **A DECLARED SPELLING THAT DIFFERS FROM THE STORED ONE BY CASE ALONE
//! IS THE SAME ROW, AND THE STORED SPELLING IS KEPT.** The path is the partition
//! key every record in the estate carries in `Meta.project_id`; rewriting it is
//! a rename by another name, and a rename is an alias and never a rewrite
//! (D53) — out of this verb's scope. If `display_name` and `source_repo` match,
//! the row counts `unchanged`.
//!
//! # No D9 key, no `expect_version`
//!
//! Idempotent by CONVERGENCE: the same declaration twice changes nothing the
//! second time. A D9 replay would be wrong here rather than redundant — after a
//! database restore, a replayed key would return the stored answer and skip the
//! convergence the restore needs. See `SyncProjectsRequest` in the contract.

use std::collections::{HashMap, HashSet};

use sqlx::{MySql, Transaction};
use tonic::Status;

use crate::path;
use crate::pb::yadgar::common::v1::Visibility;
use crate::pb::yadgar::project::v1::*;
use crate::service::{ProjectDb, SYNC_CALLER};
use crate::sql::{holes, internal, scope_of};

/// The width of `display_name` AND `source_repo`, in CHARACTERS — both are
/// `VARCHAR(255)` on utf8mb4. `register.rs`'s `MAX_DISPLAY_NAME_CHARS` makes
/// the whole argument for counting characters rather than bytes.
const MAX_CHARS: usize = 255;

/// The width of `created_by` / `updated_by`, `VARCHAR(64)`.
const MAX_PRINCIPAL_CHARS: usize = 64;

impl ProjectDb {
    /// The whole sync, for a caller whose identity has already been read.
    ///
    /// **THE GRPC HANDLER IS THE ONLY PRODUCTION CALLER**, and it passes the
    /// common name of the verified client leaf (`src/service.rs`). The identity
    /// is a parameter so the transaction can be tested without a handshake;
    /// over the wire there is no way to supply it except by presenting the
    /// certificate.
    pub async fn sync_as(
        &self,
        caller: Option<&str>,
        req: SyncProjectsRequest,
    ) -> Result<SyncProjectsResponse, Status> {
        admit(caller)?;
        let scope = scope_of(&req.scope)?;
        let actor = principal(&scope.user_id)?;
        let declared = validate(&req)?;

        let mut tx = self.pool.begin().await.map_err(internal)?;
        match apply(&mut tx, &declared, actor, req.allow_mass_archive).await {
            Ok(response) => {
                tx.commit().await.map_err(internal)?;
                Ok(response)
            }
            Err(refusal) => {
                tx.rollback().await.map_err(internal)?;
                Err(refusal)
            }
        }
    }
}

/// Plan step 1: `project-caller`, EXACTLY. The name `platform` writes is
/// lower-case, and a leaf spelled otherwise is not that leaf.
fn admit(caller: Option<&str>) -> Result<(), Status> {
    if caller == Some(SYNC_CALLER) {
        return Ok(());
    }
    Err(Status::permission_denied(format!(
        "SyncProjects is callable by `project` only (ADR-0936, ADR-0938): the client \
         certificate must name {SYNC_CALLER:?}, and this one names {}. Every module's leaf \
         verifies against the same authority, so verification alone admits nobody here",
        caller.map_or_else(|| "no caller".to_string(), |c| format!("{c:?}"))
    )))
}

/// The principal stamped on every row the sync writes (ADR-0942). Required,
/// and no wider than the column — a refusal is a sentence, a truncation is not.
fn principal(user_id: &str) -> Result<&str, Status> {
    let chars = user_id.chars().count();
    if chars == 0 || chars > MAX_PRINCIPAL_CHARS {
        return Err(Status::invalid_argument(format!(
            "scope.user_id is the principal stamped as created_by and updated_by, and must be \
             1 to {MAX_PRINCIPAL_CHARS} characters; it is {chars}"
        )));
    }
    Ok(user_id)
}

/// One declared row, after validation, with its case-folded key.
struct Declared<'a> {
    key: String,
    path: &'a str,
    display_name: &'a str,
    source_repo: &'a str,
}

/// Plan step 2: the whole request, or nothing.
fn validate(req: &SyncProjectsRequest) -> Result<Vec<Declared<'_>>, Status> {
    if req.roots.is_empty() {
        return Err(Status::invalid_argument(
            "a declaration names at least one root: the namespace anchors every project sits \
             under (ADR-0673). None was sent",
        ));
    }
    let mut out = Vec::with_capacity(req.roots.len() + req.projects.len());
    let mut roots = HashSet::new();
    for root in &req.roots {
        let row = row("roots[].path", root)?;
        if root.path.contains('/') {
            return Err(Status::invalid_argument(format!(
                "the root {:?} has more than one segment. A root is a namespace anchor, a \
                 single segment such as \"acme\"; declare a deeper path as a project",
                root.path
            )));
        }
        roots.insert(row.key.clone());
        out.push(row);
    }
    for project in &req.projects {
        let row = row("projects[].path", project)?;
        let first = row.key.split('/').next().unwrap_or_default();
        if !row.key.contains('/') || !roots.contains(first) {
            return Err(Status::invalid_argument(format!(
                "the project {:?} is not under a declared root. Every project sits strictly \
                 beneath one of the declaration's roots",
                project.path
            )));
        }
        out.push(row);
    }
    refuse_duplicates(&out)?;
    Ok(out)
}

/// One row's own checks: grammar, class, and the two column widths.
fn row<'a>(what: &'static str, d: &'a DeclaredProject) -> Result<Declared<'a>, Status> {
    path::validate(what, &d.path)?;
    path::refuse_reserved_root(what, &d.path)?;
    if path::is_private_class(&d.path) {
        return Err(Status::invalid_argument(format!(
            "{what} {:?} is a private `local/...` path. A sync declares organisation projects \
             only; a personal project belongs to one account and is registered by it",
            d.path
        )));
    }
    if d.source_repo.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{:?} has no source_repo. Every declared row is org class, and an org row names the \
             repository whose pull request flow governs it",
            d.path
        )));
    }
    for (field, value) in [
        ("source_repo", &d.source_repo),
        ("display_name", &d.display_name),
    ] {
        let chars = value.chars().count();
        if chars > MAX_CHARS {
            return Err(Status::invalid_argument(format!(
                "the {field} of {:?} is {chars} characters and the limit is {MAX_CHARS}",
                d.path
            )));
        }
    }
    Ok(Declared {
        key: d.path.to_ascii_lowercase(),
        path: &d.path,
        display_name: &d.display_name,
        source_repo: &d.source_repo,
    })
}

/// Two spellings of one path are one path to the store, so they are one path
/// here too — across roots and projects alike.
fn refuse_duplicates(rows: &[Declared<'_>]) -> Result<(), Status> {
    let mut seen = HashSet::new();
    for row in rows {
        if !seen.insert(row.key.as_str()) {
            return Err(Status::invalid_argument(format!(
                "{:?} is declared twice (paths are compared with ASCII case folded, as the store \
                 compares them)",
                row.path
            )));
        }
    }
    Ok(())
}

/// An org-class row as the store holds it.
#[derive(sqlx::FromRow)]
struct Stored {
    id: String,
    path: String,
    display_name: String,
    source_repo: Option<String>,
    status: i8,
}

/// Plan steps 3 to 7, inside the caller's transaction.
async fn apply(
    tx: &mut Transaction<'_, MySql>,
    declared: &[Declared<'_>],
    actor: &str,
    allow_mass_archive: bool,
) -> Result<SyncProjectsResponse, Status> {
    sqlx::query("SELECT id FROM project_sync_lock WHERE id = 1 FOR UPDATE")
        .fetch_one(&mut **tx)
        .await
        .map_err(internal)?;

    let stored: Vec<Stored> = sqlx::query_as(
        "SELECT id, path, display_name, source_repo, status FROM project
          WHERE owner_user_id IS NULL",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(internal)?;
    refuse_aliases(tx, declared).await?;

    let by_key: HashMap<String, &Stored> = stored
        .iter()
        .map(|s| (s.path.to_ascii_lowercase(), s))
        .collect();
    let keys: HashSet<&str> = declared.iter().map(|d| d.key.as_str()).collect();
    let active = ProjectStatus::Active as i8;
    let to_archive: Vec<&str> = stored
        .iter()
        .filter(|s| s.status == active && !keys.contains(s.path.to_ascii_lowercase().as_str()))
        .map(|s| s.id.as_str())
        .collect();
    let active_count = stored.iter().filter(|s| s.status == active).count();
    guard(to_archive.len(), active_count, allow_mass_archive)?;

    let mut response = SyncProjectsResponse::default();
    for d in declared {
        write_one(tx, d, by_key.get(&d.key).copied(), actor, &mut response).await?;
    }
    archive(tx, &to_archive, actor).await?;
    response.archived = to_archive.len() as u32;
    Ok(response)
}

/// Plan step 5. One read for the whole declaration; the column folds case.
async fn refuse_aliases(
    tx: &mut Transaction<'_, MySql>,
    declared: &[Declared<'_>],
) -> Result<(), Status> {
    let sql = format!(
        "SELECT alias_path FROM project_alias WHERE alias_path IN ({})",
        holes(declared.len())
    );
    // AUDIT: the only interpolation is `holes`, a run of `?`.
    let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql));
    for d in declared {
        query = query.bind(d.path);
    }
    let Some(alias) = query.fetch_optional(&mut **tx).await.map_err(internal)? else {
        return Ok(());
    };
    let folded = alias.to_ascii_lowercase();
    let path = declared
        .iter()
        .find(|d| d.key == folded)
        .map_or(alias.as_str(), |d| d.path);
    Err(Status::already_exists(format!(
        "{path:?} is a former path of another project and still resolves to it (D53). \
         Declaring a project there would make one path mean two things"
    )))
}

/// Plan step 6 (ADR-0939): STRICTLY more than half of the active org-class
/// rows, roots included, is refused unless the caller said otherwise. Exactly
/// half passes; an empty store has nothing to archive and never trips it.
fn guard(archiving: usize, active: usize, allowed: bool) -> Result<(), Status> {
    if allowed || archiving * 2 <= active {
        return Ok(());
    }
    Err(Status::failed_precondition(format!(
        "this sync would archive {archiving} of the {active} active organisation projects, \
         which is more than half (ADR-0939), and nothing was written. A truncated or \
         mis-rendered declaration looks exactly like this. If the archive is intended, send the \
         same declaration with allow_mass_archive set"
    )))
}

/// Insert, overwrite, reactivate, or leave one declared row — and count it.
async fn write_one(
    tx: &mut Transaction<'_, MySql>,
    d: &Declared<'_>,
    stored: Option<&Stored>,
    actor: &str,
    response: &mut SyncProjectsResponse,
) -> Result<(), Status> {
    let Some(stored) = stored else {
        insert(tx, d, actor).await?;
        response.inserted += 1;
        return Ok(());
    };
    let differs = stored.display_name != d.display_name
        || stored.source_repo.as_deref() != Some(d.source_repo);
    let reactivating = stored.status != ProjectStatus::Active as i8;
    if !differs && !reactivating {
        response.unchanged += 1;
        return Ok(());
    }
    // ONE STATEMENT FOR BOTH: a reactivated row is overwritten too if it also
    // differed, and counts once — as reactivated.
    sqlx::query(
        "UPDATE project
            SET display_name = ?, source_repo = ?, status = ?, version = version + 1,
                updated_by = ?, updated_at = CURRENT_TIMESTAMP
          WHERE id = ?",
    )
    .bind(d.display_name)
    .bind(d.source_repo)
    .bind(ProjectStatus::Active as i8)
    .bind(actor)
    .bind(&stored.id)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    if reactivating {
        response.reactivated += 1;
    } else {
        response.updated += 1;
    }
    Ok(())
}

async fn insert(
    tx: &mut Transaction<'_, MySql>,
    d: &Declared<'_>,
    actor: &str,
) -> Result<(), Status> {
    // UUIDv7 in a URN, as `register` mints one (D42).
    let id = format!("yadgar:project:{}", uuid::Uuid::now_v7());
    sqlx::query(
        "INSERT INTO project
           (id, version, path, display_name, status, source_repo, owner_user_id, visibility,
            created_by, updated_by)
         VALUES (?, 1, ?, ?, ?, ?, NULL, ?, ?, ?)",
    )
    .bind(&id)
    .bind(d.path)
    .bind(d.display_name)
    .bind(ProjectStatus::Active as i8)
    .bind(d.source_repo)
    .bind(Visibility::Org as i8)
    .bind(actor)
    .bind(actor)
    .execute(&mut **tx)
    .await
    .map_err(internal)?;
    Ok(())
}

/// A STATUS FLIP, NEVER A DELETE (ADR-0934). The row, its aliases and every
/// record stamped with its path stay; re-declaring the path reactivates it.
async fn archive(tx: &mut Transaction<'_, MySql>, ids: &[&str], actor: &str) -> Result<(), Status> {
    if ids.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE project
            SET status = ?, version = version + 1, updated_by = ?,
                updated_at = CURRENT_TIMESTAMP
          WHERE id IN ({})",
        holes(ids.len())
    );
    // AUDIT: the only interpolation is `holes`, a run of `?`.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(ProjectStatus::Archived as i8)
        .bind(actor);
    for id in ids {
        query = query.bind(*id);
    }
    query.execute(&mut **tx).await.map_err(internal)?;
    Ok(())
}
