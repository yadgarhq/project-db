//! What a project path is, and the chain of ancestors a resolution walks.
//!
//! **THIS IS THE MODULE'S SUBJECT, not a validation helper beside it.** D53 says
//! a project id is a slash-separated path whose depth carries meaning, and D52
//! says this module is what decides which such values are real. Everything else
//! here is storage; the grammar is the decision.
//!
//! **ADR-0227: AN IDENTITY IS NEVER DERIVED.** Nothing in this file guesses,
//! trims, lowercases, or repairs a path. A caller supplies one that satisfies
//! the grammar or the call is refused with the reason. Normalising would be the
//! same failure one layer down from the one the registry exists to remove: two
//! spellings quietly becoming one scope is as bad as one spelling quietly
//! becoming two.
//!
//! **THE GRAMMAR IS DELIBERATELY NARROWER THAN "A STRING WITH SLASHES IN IT",
//! and the reason is measured rather than aesthetic.** `plans/client-agent-agnostic.md`
//! records what a required free-text parameter actually receives: yadgar's own
//! corpus carries eighteen distinct `directory_context` values that are PROSE
//! rather than paths — `"debugging opsecrets nixos-quinyx"` and its like —
//! because a parameter's name invites a description. Every one of those contains
//! a space. Refusing anything outside `[A-Za-z0-9._-]` per segment is the
//! cheapest guard that catches the whole observed class, and it costs nothing a
//! real repository or service directory name needs.
//!
//! # What is NOT enforced here, and why not
//!
//! **THERE IS NO DEPTH CAP.** D53 says depth is capped and that the limit is
//! CONFIGURATION (D43), and ADR-0569 says a configuration knob has no
//! compiled-in default — a value nobody chose, used as if somebody had, is
//! exactly what that rule exists to delete. So the cap needs a line in
//! `yadgarhq/config`'s `project-db.yaml`, that file is deliberately empty in the
//! change that introduces this module, and inventing a constant here to stand in
//! for it would be the defect rather than a placeholder for the fix. What bounds
//! a path today is its LENGTH: [`MAX_LEN`] is the width of the column that
//! stores it, so a path longer than the store can hold is refused with a
//! sentence instead of being truncated by the engine. That is a length bound and
//! it is not a depth cap; the cap is filed rather than improvised.
//!
//! **AND THE PATHS THIS STORE MUST ACCEPT ARE DEEP.** The private class of
//! project ids is keyed on the FULL directory path rather than on its basename,
//! because `local/<basename>` collides between two unrelated directories that
//! happen to share a name — and a collision here is two projects sharing one
//! partition key, which is the failure this whole module exists to prevent. So a
//! real id carries a segment per directory, the client TRIMS FOR LENGTH rather
//! than for depth, and a depth cap invented here would refuse an id the client
//! legitimately produced.

use tonic::Status;

/// The width of `project.path` and of `project_alias.alias_path`.
///
/// **THE CHECK EXISTS SO THE REFUSAL IS A SENTENCE.** Without it the engine
/// decides: MariaDB in strict mode answers "Data too long for column 'path'",
/// which names a column a caller has never heard of, and in a non-strict mode it
/// TRUNCATES — which mints a second project silently, the precise failure D52
/// describes. One number, checked here, and it is the same number the migration
/// declares.
pub const MAX_LEN: usize = 255;

/// The one segment RESERVED from the organisation namespace.
///
/// **THE PRIVATE CLASS OF PROJECT IDS LIVES UNDER THIS SEGMENT, WHICH IS
/// PRECISELY WHY NOBODY MAY OWN IT.** A directory that is not a repository is
/// registered beneath it and is visible only to the account that owns it; an
/// organisation project is `<org>/<repo>` and is visible to everyone. The two
/// classes share one namespace and are told apart by the FIRST SEGMENT alone.
///
/// **A REGISTRATION AT THE BARE SEGMENT WOULD SWALLOW THE WHOLE CLASS.** An
/// unregistered path resolves to its nearest registered ANCESTOR rather than
/// failing (D52 as amended by D53) — so with a row at `local`, every private
/// path in the estate resolves to it with `exact: false`, and every scoped write
/// is then stamped with one partition key belonging to whoever registered it
/// first. That is the split-corpus failure of D52 arriving through the root of a
/// namespace instead of through a typo.
///
/// **AND THE DOOR ONLY OPENS ONE WAY, which is why the guard lands before the
/// first caller rather than after.** A registration is immutable in this estate:
/// renaming is forbidden because every record already stamped with a path goes
/// on carrying it, so `RenameProject` answers `UNIMPLEMENTED` (`crate::write`)
/// and archiving is the only retirement there is. A `local` claimed by anybody
/// could never be renamed away.
pub const RESERVED_ROOT: &str = "local";

/// Refuse the bare [`RESERVED_ROOT`], and only the bare segment.
///
/// **`local/<anything>` STAYS REGISTRABLE, and that is the whole precision of
/// this check.** The reservation is about the ORGANISATION SEGMENT, not about
/// the prefix: `local/home/max/git/alpha` IS the private class, and refusing it
/// would delete the class the segment is reserved to carry. The comparison is
/// equality rather than `starts_with` for the same reason [`ancestors`] splits
/// on `/`: `locals` is a different organisation that merely shares five
/// characters.
///
/// **THE EQUALITY IS ASCII-CASE-INSENSITIVE, AND THE STORE IS WHAT DECIDES
/// THAT.** `project.path` and `project_alias.alias_path` collate
/// `utf8mb4_general_ci` (`crate::schema`, migration 4), which is case- AND
/// accent-insensitive. That is DECLARED rather than inherited: migrations 1 and
/// 2 named no `COLLATE`, so until migration 4 the columns took
/// `@@collation_server` and this guard rested on a setting no deployment
/// states. `uq_project_path`
/// therefore holds ONE slot for every ASCII-case spelling of the segment, so a
/// byte-exact comparison here lets `LOCAL` pass the guard, reach the INSERT and
/// occupy the reserved slot — permanently, because the retirement the paragraph
/// above names is not shipped either: `crate::write` holds BOTH `RenameProject`
/// and `ArchiveProject` at `UNIMPLEMENTED` in this release. Folding case is what
/// closes that; the door is meant to open one way and this is the half of it a
/// byte comparison left open.
///
/// **ASCII CASE IS THE WHOLE OF WHAT THE COLLATION FOLDS HERE, and that is a
/// property of the GRAMMAR rather than of this line.** The accent-insensitive
/// half has nothing to act on, because [`validate`] runs before this at both
/// call sites and admits only `[A-Za-z0-9._-]` — no accented character ever
/// reaches the comparison. Measured on the same engine, within that alphabet:
/// `lo-cal`, `l.ocal` and `lo_cal` all compare UNEQUAL to `local`, and `LoCaL`
/// compares equal. So no migration is needed to make this safe, and
/// `the_segment_alphabet_is_ascii_only_which_is_what_bounds_the_collation_guard`
/// reddens if anybody widens the alphabet out from under that reasoning.
///
/// **IT IS NOT FOLDED INTO [`validate`], AND THAT IS A DECISION RATHER THAN A
/// PLACEMENT.** [`validate`] asks whether a value is a project path at all, and
/// `local` is a perfectly well-formed one; this asks whether a well-formed path
/// may be OWNED. Folding the two together would apply the refusal to every
/// caller of [`validate`] — including `TouchProjects`, which validates each path
/// in a batch and refuses the whole flush on the first failure, on the stated
/// ground that one bad path must never block the flush for every other project
/// for ever. So the check is opt-in per call site, and the sites that take it
/// are the two that can MINT or FOLLOW an ownership claim: `register` and
/// `resolve`. `GetProject` and `ListProjects` deliberately do not, because an
/// operator holding a store that already contains such a row needs to be able to
/// see it.
///
/// **FAIL LOUD, NEVER COERCE (ADR-0569).** The refusal names the value rather
/// than quietly rewriting it into something registrable: a silently rewritten
/// identity is the same failure one layer down from the one this module exists
/// to remove.
pub fn refuse_reserved_root(what: &'static str, path: &str) -> Result<(), Status> {
    if path.eq_ignore_ascii_case(RESERVED_ROOT) {
        // THE CALLER'S OWN SPELLING, not the constant. Answering `LOCAL` with a
        // message that says `"local"` tells somebody their value was something
        // it was not, which is the quiet rewriting of an identity ADR-0569
        // refuses. Safe to echo: anything reaching this branch is the length of
        // `RESERVED_ROOT`, so the no-echo rule `validate` applies to an
        // over-long value has nothing to protect here.
        return Err(Status::invalid_argument(format!(
            "{what} is {path:?}, which is the RESERVED segment {RESERVED_ROOT:?} and is not an \
             organisation — the store folds ASCII case, so every spelling of it is one key. \
             It is the first segment of the private class of project ids — a directory that is \
             not a repository is registered beneath it — so a project owning it would be the \
             nearest registered ancestor of every private path in the estate, and every one of \
             them would resolve into it (D52, D53). Name the project itself, such as \
             \"{RESERVED_ROOT}/home/you/src/thing\", or an organisation of your own"
        )));
    }
    Ok(())
}

/// Whether *path* is in the PRIVATE class — `local/<account>/<path>` (ADR-0605).
///
/// **THE PREDICATE THE ENGINE'S OWN CHECK APPLIES, spelled in Rust.**
/// `ck_project_class_path` (`crate::schema`, migration 1) is
/// `(owner_user_id IS NOT NULL) = (path LIKE 'local/%')`, evaluated under
/// `utf8mb4_general_ci`, which folds ASCII case. This must agree with it exactly:
/// a row the Rust side classifies as private and the engine classifies as
/// organisational is refused with a CHECK violation, which reaches a caller as
/// `Status::internal` naming nothing (`sql::internal`).
///
/// **THE SEGMENT, NEVER THE PREFIX**, for the reason
/// [`refuse_reserved_root`] gives about `locals`: splitting on `/` is what makes
/// `local-mirror` a different organisation rather than a private path. And ASCII
/// case is the whole of what either side folds, because [`validate`] admits only
/// `[A-Za-z0-9._-]`.
///
/// **THE BARE SEGMENT IS NOT IN THE CLASS, and the two rules agree on that too.**
/// `local` does not match `local/%`, and it has no `/` to split on — so this
/// answers `false`, the engine agrees, and the row is org-class. Nothing can
/// register it in any case: [`refuse_reserved_root`] refuses it ahead of every
/// caller that could mint one.
pub fn is_private_class(path: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(root, _)| root.eq_ignore_ascii_case(RESERVED_ROOT))
}

/// Every character a path segment may contain.
///
/// Stated as a set rather than as "not these": an allowlist that meets an
/// unexpected character refuses it, and a denylist admits it. A path is a
/// partition key, and admitting something nobody considered is how the corpus
/// splits in two.
fn segment_is_legal(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Refuse anything that is not a project path.
///
/// `what` names the field being checked, so a caller reading the refusal knows
/// which of `from_path` and `to_path` it is about.
pub fn validate(what: &'static str, path: &str) -> Result<(), Status> {
    if path.is_empty() {
        return Err(Status::invalid_argument(format!(
            "{what} is required and is never derived: a project path is supplied by the caller \
             or the call is refused (ADR-0227). It is a slash-separated path such as \
             \"acme/forecast\" (D53)"
        )));
    }
    // BEFORE the value is echoed anywhere below. A refusal that interpolates an
    // unbounded caller string into a message is a caller deciding how large this
    // service's log lines are.
    if path.len() > MAX_LEN {
        return Err(Status::invalid_argument(format!(
            "{what} is {} characters and the limit is {MAX_LEN}. The value is not repeated here \
             because it is longer than any message this service writes",
            path.len()
        )));
    }
    if path.starts_with('/') || path.ends_with('/') {
        return Err(Status::invalid_argument(format!(
            "{what} must not begin or end with a slash: {path:?}. A project path names a \
             hierarchy of segments (D53), and a leading or trailing slash is an empty segment"
        )));
    }
    for segment in path.split('/') {
        if !segment_is_legal(segment) {
            return Err(Status::invalid_argument(format!(
                "{what} contains the segment {segment:?}, which is not a project path segment: \
                 {path:?}. A segment is one or more of A-Z a-z 0-9 . _ - and is neither \".\" \
                 nor \"..\". Spaces and punctuation are refused because a required free-text \
                 identifier attracts a DESCRIPTION, and a description accepted as an identity \
                 mints a project nobody meant to create (D52)"
            )));
        }
    }
    Ok(())
}

/// The path and every ancestor of it, DEEPEST FIRST.
///
/// `a/b/c` yields `["a/b/c", "a/b", "a"]`. The order is the order a resolution
/// prefers them in, so the caller of this function does not re-derive it.
///
/// **SEGMENT-WISE, NEVER BY STRING PREFIX, and that difference is the whole
/// point of the function existing at all.** A registered `acme/team` is NOT an
/// ancestor of `acme/teamx/svc`, and a `starts_with` implementation says it is —
/// silently filing one project's work under a project that merely shares its
/// first seven characters. Splitting on `/` is what makes the hierarchy semantic
/// (D53) rather than a naming convention.
pub fn ancestors(path: &str) -> Vec<&str> {
    let mut out = vec![path];
    let mut rest = path;
    while let Some(cut) = rest.rfind('/') {
        rest = &rest[..cut];
        out.push(rest);
    }
    out
}

#[cfg(test)]
mod tests;
