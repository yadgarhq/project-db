//! How a caller's `Scope` is used here — and, unusually for a `-db`, how it is
//! not — and how an engine error becomes a status.

use tonic::Status;

use crate::pb::yadgar::common::v1::Scope;

/// The `Scope` a WRITE requires: `RegisterProject` and `TouchProjects` call
/// this. (`SyncProjects` has its own check, whose message fits its caller.)
///
/// The gateway attests scope from credentials; it is never supplied by the
/// caller (D12). An absent scope on a write is a programming error upstream,
/// not a permissive default — so it is refused rather than treated as
/// "everything". `RegisterProject` stamps `created_by` and `updated_by` from
/// `Scope.user_id` and keys D9's idempotency ledger by it. `TouchProjects`
/// reads no field of it today and keeps the requirement anyway: ledger 762
/// relaxed the reads only.
///
/// **THE THREE READS DO NOT CALL THIS (ledger 762, route (c) of
/// `plans/the-project-logic-tier.md` §6).** `ResolveProject`, `GetProject` and
/// `ListProjects` accept an absent `Scope`, because no read filters, keys or
/// partitions on one — see the module documentation on [`crate::service`] —
/// and D67's id has a header fallback in [`tel_scope`]. `project` holds no
/// `Scope` on `ListProjects` or `ResolveProject` and sends none. Pinned by
/// `tests/list.rs`'s `list_accepts_an_absent_scope`, `tests/resolve.rs`'s
/// `an_absent_scope_is_accepted` and `tests/scope.rs`.
pub fn scope_of(scope: &Option<Scope>) -> Result<&Scope, Status> {
    scope
        .as_ref()
        .ok_or_else(|| Status::invalid_argument("scope is required and is attested by the gateway"))
}

/// Copy the scope fields a record needs, before the request is consumed.
///
/// Empty strings when the scope is absent: a write is refused on its own
/// merits, a read proceeds, and telemetry must never be the thing that fails a
/// request (D67).
///
/// **`request_id` FALLS BACK TO THE HEADER WHEN THE `Scope` CARRIES NONE**
/// (ledger 1254). `header` is [`request_id_of`]'s read of
/// `x-yadgar-request-id`. `project`'s `ListProjects` and `ResolveProject` carry
/// no `Scope`, so `project` sends none here and D67's id arrives in the header
/// instead — the shape `iam` → `iam-db` already
/// uses. An id the `Scope` does carry wins: it was attested by the gateway, the
/// stronger claim, and `project` resolves the two in the same order.
pub fn tel_scope(scope: &Option<Scope>, header: String) -> yadgar_telemetry::observe::Scope {
    let field = |f: fn(&Scope) -> &String| scope.as_ref().map(f).cloned().unwrap_or_default();
    let request_id = match field(|s| &s.request_id) {
        attested if !attested.is_empty() => attested,
        _ => header,
    };
    yadgar_telemetry::observe::Scope {
        request_id,
        instance_id: field(|s| &s.instance_id),
        user_id: field(|s| &s.user_id),
        project_id: field(|s| &s.project_id),
    }
}

/// How many of a caller's characters `x-yadgar-request-id` may put on a record.
///
/// A cut value is at most `REQUEST_ID_CAP + 3` characters, the marker included.
///
/// **THE SAME BOUND AND MARKER `project`, `iam` AND `iam-db` APPLY** (their
/// `REQUEST_ID_CAP`, `CUT_MARKER` and `capped`). A copy rather than a shared
/// crate because none holds it yet — TODO(1255): replace this copy, the marker
/// and [`capped`] with the shared crate's when ledger 1255 lands. Until then the
/// copies must stay identical, or two hops cut one over-long id differently and
/// their records stop joining.
const REQUEST_ID_CAP: usize = 256;

/// What marks a cut. ASCII, so a capped id is a valid header value on every hop.
const CUT_MARKER: &str = "...";

/// `s` cut to [`REQUEST_ID_CAP`] characters, with [`CUT_MARKER`] appended if it
/// was cut. Capping a capped value changes nothing, so an id `project` already
/// capped is recorded here unchanged and the two records join.
fn capped(s: &str) -> std::borrow::Cow<'_, str> {
    match s.char_indices().nth(REQUEST_ID_CAP) {
        Some((at, _)) => format!("{}{CUT_MARKER}", &s[..at]).into(),
        None => s.into(),
    }
}

/// D67's join key as `project` forwards it, in `x-yadgar-request-id` (ledger
/// 1254). Read BEFORE the request is consumed: `into_inner` drops the metadata.
///
/// Capped rather than refused, and an absent or unreadable value is `""`,
/// because a correlation id must never fail a call (D67). A header is bounded
/// only by the transport, and this `String` reaches the `CallRecord`.
pub fn request_id_of<T>(req: &tonic::Request<T>) -> String {
    req.metadata()
        .get("x-yadgar-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|v| capped(v).into_owned())
        .unwrap_or_default()
}

/// Neutralise the LIKE metacharacters in a value that is about to become part of
/// a pattern.
///
/// A bound parameter stops SQL injection; it does NOT stop PATTERN injection.
/// `_` matches any single character, so an unescaped `acme_team/%` also matches
/// `acmeXteam/secret` — a different project entirely. `%` is the same hole at its
/// widest: an `under_path` of `%` would list the whole registry.
///
/// **THE GRAMMAR DOES NOT MAKE THIS REDUNDANT.** `_` is a legal segment
/// character — `crate::path` admits it, because repository names contain it —
/// so the collision is reachable with two perfectly valid paths and no hostile
/// caller at all. `%` is refused by the grammar today and is escaped anyway: the
/// escaping must not depend on a second file's rules staying as they are.
///
/// The backslash goes FIRST. Escaping it after the others would escape the
/// escapes.
fn like_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The LIKE pattern matching everything strictly BENEATH a path (D53).
///
/// The `/%` is appended AFTER escaping, so the wildcard this pattern is supposed
/// to have survives while the ones a value smuggled in do not. The trailing
/// slash is what makes it a subtree rather than a prefix: without it `acme/team`
/// would match `acme/teamx`, which is a different project.
pub fn subtree(path: &str) -> String {
    format!("{}/%", like_escape(path))
}

/// Spelled out rather than left to MariaDB's default, which `NO_BACKSLASH_ESCAPES`
/// changes underneath a statement that assumed it.
pub const ESCAPE: &str = r"ESCAPE '\\'";

/// An engine error is never returned to the caller verbatim: it carries table
/// names, column names and sometimes values. Logged here, generic on the wire.
///
/// EXCEPT for the two classes a caller can act on. A deadlock or a lock wait
/// timeout means "someone else held the row; try again", and flattening it into
/// `INTERNAL` tells a caller that a retryable wait is a permanent failure. A pool
/// that had nothing to hand out is the same argument one layer out, and is
/// `UNAVAILABLE` rather than `ABORTED` because nothing was serialised against
/// anything — the request never reached the engine.
pub fn internal(e: sqlx::Error) -> Status {
    if matches!(e, sqlx::Error::PoolTimedOut) {
        tracing::warn!(error = %e, "project-db pool exhausted; the caller may retry");
        return Status::unavailable("no connection was free in time — retry");
    }
    if let sqlx::Error::Database(db) = &e {
        // 1213 is ER_LOCK_DEADLOCK and 1205 ER_LOCK_WAIT_TIMEOUT. SQLSTATE
        // 40001 covers the first; the second reports HY000 and is only
        // identifiable by its number.
        let deadlock = db.code().as_deref() == Some("40001")
            || db
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .is_some_and(|e| matches!(e.number(), 1205 | 1213));
        if deadlock {
            tracing::warn!(error = %e, "project-db lock contention; the caller may retry");
            return Status::aborted("the write could not be serialised — retry");
        }
    }
    tracing::error!(error = %e, "project-db engine error");
    Status::internal("storage error")
}

/// `?, ?, ?` for a list of `n` bound values.
///
/// Rendering `IN ()` is a syntax error, so every caller checks for the empty
/// case before reaching this — which is why it takes a non-zero count rather
/// than answering with something an empty list could produce.
pub fn holes(n: usize) -> String {
    debug_assert!(n > 0, "IN () is a syntax error; check the empty case first");
    vec!["?"; n].join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SENTINEL ROOT — see `crate::path`'s tests for why it is not one of the
    /// contract's own worked examples.
    const R: &str = "pangolin-7c21";

    /// **THE COLLISION IS REACHABLE WITH TWO LEGAL PATHS**, which is what makes
    /// this a real guard rather than a defence against a caller nobody has. `_`
    /// is admitted by the grammar because repository names contain it.
    #[test]
    fn a_subtree_pattern_neutralises_the_underscore_wildcard() {
        let pattern = subtree(&format!("{R}/a_b"));
        assert_eq!(pattern, format!("{R}/a\\_b/%"));
        assert!(
            !pattern.contains("a_b"),
            "an unescaped underscore matches any single character, so {R}/axb/svc would be \
             listed under {R}/a_b: {pattern}"
        );
    }

    #[test]
    fn a_subtree_pattern_is_a_subtree_and_not_a_prefix() {
        // Without the slash, `{R}/team/%` would be `{R}/team%` and would match
        // `{R}/teamx` — a sibling project, not a descendant.
        assert_eq!(subtree(&format!("{R}/team")), format!("{R}/team/%"));
    }

    #[test]
    fn the_backslash_is_escaped_before_the_metacharacters_it_would_otherwise_escape() {
        // `\_` arriving as data must become `\\\_`, not `\\_` — the latter is a
        // literal backslash followed by a live single-character wildcard.
        assert_eq!(like_escape(r"a\_b"), r"a\\\_b");
    }

    #[test]
    fn a_pool_timeout_is_unavailable_and_therefore_retryable() {
        assert_eq!(
            internal(sqlx::Error::PoolTimedOut).code(),
            tonic::Code::Unavailable,
            "every connection being busy is the most transient condition this service has, and \
             must not be reported as a permanent failure"
        );
    }

    #[test]
    fn any_other_engine_error_is_still_an_opaque_internal() {
        let status = internal(sqlx::Error::RowNotFound);
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "storage error");
    }
}
