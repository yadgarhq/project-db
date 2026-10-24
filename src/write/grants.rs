//! The grant half of `SyncProjects` (ADR-0962, ADR-0963 (1) and (6),
//! `plans/team-access.md` stage 4): validate each declared row's grants, plan
//! the writes against the stored set, guard mass revocation, and write.
//!
//! A grant is (row, team, level). The store converges to the declared set: a
//! declared pair not stored is inserted, a stored pair not declared is deleted
//! (every grant of an archived row included), and a kept pair whose level
//! changed is rewritten in place. A grant change never bumps the row's
//! `version`.

use std::collections::{HashMap, HashSet};

use sqlx::{MySql, Transaction};
use tonic::Status;

use super::sync::Declared;
use crate::pb::yadgar::project::v1::*;
use crate::sql::internal;

/// A grant's team id: `yadgar:team:` and a declared team name, which matches
/// `^[a-z][a-z0-9-]{0,62}$` (`plans/team-access.md` §2).
const TEAM_PREFIX: &str = "yadgar:team:";
const MAX_TEAM_NAME_CHARS: usize = 63;

/// One row's grants: every refusal names the path and the team, and is
/// decided before anything is opened, so it writes nothing.
pub(super) fn validate(d: &DeclaredProject) -> Result<Vec<(&str, i8)>, Status> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(d.grants.len());
    for g in &d.grants {
        let level = match GrantLevel::try_from(g.level) {
            Ok(GrantLevel::Read) => GrantLevel::Read as i8,
            Ok(GrantLevel::ReadWrite) => GrantLevel::ReadWrite as i8,
            Ok(GrantLevel::Unspecified) | Err(_) => {
                return Err(Status::invalid_argument(format!(
                    "the grant to {:?} on {:?} has level {}, which is not read (1) or \
                     read-write (2). GRANT_LEVEL_UNSPECIFIED is refused, never stored as read",
                    g.team_id, d.path, g.level
                )));
            }
        };
        if !is_team_id(&g.team_id) {
            return Err(Status::invalid_argument(format!(
                "the grant on {:?} names {:?}, which is not a team id. A team id is \
                 {TEAM_PREFIX:?} followed by a declared team name: a lowercase letter, then up to \
                 62 lowercase letters, digits or hyphens",
                d.path, g.team_id
            )));
        }
        if !seen.insert(g.team_id.as_str()) {
            return Err(Status::invalid_argument(format!(
                "{:?} grants {:?} twice. One team appears at most once per row",
                d.path, g.team_id
            )));
        }
        out.push((g.team_id.as_str(), level));
    }
    Ok(out)
}

fn is_team_id(id: &str) -> bool {
    let Some(name) = id.strip_prefix(TEAM_PREFIX) else {
        return false;
    };
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.chars().count() <= MAX_TEAM_NAME_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A stored grant on an org row, with its row's case-folded path.
#[derive(sqlx::FromRow)]
pub(super) struct StoredGrant {
    project_id: String,
    path: String,
    team_id: String,
    level: i8,
}

/// Every stored grant on an org row, read after the sync's lock. No grant can
/// name a private row, and a sync never touches one.
pub(super) async fn stored(tx: &mut Transaction<'_, MySql>) -> Result<Vec<StoredGrant>, Status> {
    sqlx::query_as(
        "SELECT g.project_id, p.path, g.team_id, g.level FROM project_grant g
           JOIN project p ON p.id = g.project_id
          WHERE p.owner_user_id IS NULL",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(internal)
}

/// What the grant half of a sync will write, decided before any write.
pub(super) struct GrantPlan<'a> {
    /// `(declared row key, team_id, level)`; the row may not exist yet.
    pub(super) inserts: Vec<(&'a str, &'a str, i8)>,
    /// `(project_id, team_id, level)` of a kept pair whose level changed.
    pub(super) updates: Vec<(&'a str, &'a str, i8)>,
    /// `(project_id, team_id)` of a stored pair the declaration does not name.
    pub(super) deletes: Vec<(&'a str, &'a str)>,
    pub(super) unchanged: u32,
}

pub(super) fn plan<'a>(declared: &'a [Declared<'a>], stored: &'a [StoredGrant]) -> GrantPlan<'a> {
    let wanted: HashMap<(&str, &str), i8> = declared
        .iter()
        .flat_map(|d| d.grants.iter().map(|(t, l)| ((d.key.as_str(), *t), *l)))
        .collect();
    let mut plan = GrantPlan {
        inserts: Vec::new(),
        updates: Vec::new(),
        deletes: Vec::new(),
        unchanged: 0,
    };
    let mut kept: HashSet<(String, &str)> = HashSet::new();
    for g in stored {
        let key = g.path.to_ascii_lowercase();
        match wanted.get(&(key.as_str(), g.team_id.as_str())) {
            Some(level) if *level == g.level => plan.unchanged += 1,
            Some(level) => plan
                .updates
                .push((g.project_id.as_str(), g.team_id.as_str(), *level)),
            None => plan
                .deletes
                .push((g.project_id.as_str(), g.team_id.as_str())),
        }
        kept.insert((key, g.team_id.as_str()));
    }
    for d in declared {
        for (team, level) in &d.grants {
            if !kept.contains(&(d.key.clone(), *team)) {
                plan.inserts.push((d.key.as_str(), team, *level));
            }
        }
    }
    plan
}

/// ADR-0963 (6): deleting STRICTLY more than half of the stored grant rows is
/// refused unless the caller said otherwise. Exactly half passes; a store with
/// no grants never trips it. A level change is not a delete.
pub(super) fn revoke_guard(deleting: usize, stored: usize, allowed: bool) -> Result<(), Status> {
    if allowed || deleting * 2 <= stored {
        return Ok(());
    }
    Err(Status::failed_precondition(format!(
        "this sync would delete {deleting} of the {stored} stored team grants, which is more \
         than half (ADR-0963 (6)), and nothing was written. A truncated or mis-rendered \
         declaration looks exactly like this. If the revocation is intended, send the same \
         declaration with allow_mass_revoke set"
    )))
}

pub(super) async fn write(
    tx: &mut Transaction<'_, MySql>,
    plan: &GrantPlan<'_>,
    ids: &HashMap<&str, String>,
) -> Result<(), Status> {
    for (project_id, team_id) in &plan.deletes {
        sqlx::query("DELETE FROM project_grant WHERE project_id = ? AND team_id = ?")
            .bind(*project_id)
            .bind(*team_id)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
    }
    for (project_id, team_id, level) in &plan.updates {
        sqlx::query("UPDATE project_grant SET level = ? WHERE project_id = ? AND team_id = ?")
            .bind(*level)
            .bind(*project_id)
            .bind(*team_id)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
    }
    for (key, team_id, level) in &plan.inserts {
        let project_id = ids
            .get(key)
            .ok_or_else(|| Status::internal(format!("no row id for the declared path {key:?}")))?;
        sqlx::query("INSERT INTO project_grant (project_id, team_id, level) VALUES (?, ?, ?)")
            .bind(project_id)
            .bind(*team_id)
            .bind(*level)
            .execute(&mut **tx)
            .await
            .map_err(internal)?;
    }
    Ok(())
}
