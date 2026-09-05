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
mod tests {
    use super::*;

    /// A SENTINEL ROOT. Nothing in this crate could produce it, and it is
    /// deliberately not `quinyx/forecast` or `quinyx/qwfm/forecast` — those are
    /// the contract's own worked examples, so a test built on them can be
    /// satisfied by an implementation that special-cases the documentation.
    const R: &str = "pangolin-7c21";

    #[test]
    fn an_ancestor_chain_is_deepest_first_and_ends_at_the_root_segment() {
        let deep = format!("{R}/wfm/forecast");
        assert_eq!(
            ancestors(&deep),
            vec![deep.as_str(), &format!("{R}/wfm"), R]
        );
        assert_eq!(ancestors(R), vec![R]);
    }

    /// **THE ONE CASE A STRING-PREFIX IMPLEMENTATION FAILS.** Every other
    /// assertion in this file passes for `starts_with`; this is the fixture that
    /// makes the difference between a hierarchy and a substring observable.
    #[test]
    fn a_sibling_sharing_a_prefix_is_not_an_ancestor() {
        let candidate = format!("{R}/teamx/svc");
        let chain = ancestors(&candidate);
        assert!(
            !chain.contains(&format!("{R}/team").as_str()),
            "{R}/team is not an ancestor of {R}/teamx/svc; a prefix is not a hierarchy: {chain:?}"
        );
        assert!(chain.contains(&R), "the root segment is still an ancestor");
    }

    #[test]
    fn a_single_segment_is_a_legal_path() {
        // Refusing it would forbid registering the very root that a typo in a
        // deeper path is supposed to resolve UP to (D52).
        validate("path", R).expect("a root project is a project");
    }

    #[test]
    fn prose_is_refused_and_the_refusal_names_the_segment() {
        // THE MEASURED CLASS, not an invented one: a `directory_context` value
        // from yadgar's own corpus, which is a description sitting where an
        // identity belongs.
        let err = validate("path", "debugging opsecrets nixos-quinyx")
            .expect_err("a description is not an identity");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains("debugging opsecrets nixos-quinyx"),
            "the refusal must name the segment it refused: {}",
            err.message()
        );
    }

    #[test]
    fn the_shapes_that_are_not_paths_are_refused() {
        for bad in [
            "",
            "/leading",
            "trailing/",
            "double//segment",
            "with space/x",
            "wild%card/x",
            "under\tscore-is-fine-but-not-a-tab/x",
            "dot/./x",
            "up/../x",
        ] {
            let err = validate("path", bad).expect_err("{bad:?} is not a project path");
            assert_eq!(err.code(), tonic::Code::InvalidArgument, "{bad:?}");
        }
    }

    /// `_` IS LEGAL and is a LIKE metacharacter, which is why `sql::subtree`
    /// escapes rather than assuming the grammar keeps them out. Pinned here so
    /// that narrowing the grammar cannot silently retire that escaping.
    #[test]
    fn an_underscore_is_a_legal_segment_character() {
        validate("path", &format!("{R}/a_b")).expect("an underscore is legal in a repository name");
    }

    #[test]
    fn a_path_wider_than_the_column_is_refused_without_being_echoed() {
        let long = format!("{R}/{}", "x".repeat(MAX_LEN));
        let err = validate("path", &long).expect_err("wider than the column");
        assert!(
            !err.message().contains(&long),
            "an over-long value must not be interpolated into the refusal: {}",
            err.message()
        );
        assert!(err.message().contains("255"), "{}", err.message());
    }
}
