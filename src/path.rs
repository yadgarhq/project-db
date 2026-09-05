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
/// THAT.** `project.path` and `project_alias.alias_path` are declared
/// `DEFAULT CHARSET=utf8mb4` with no `COLLATE` (`crate::schema`, migration 1),
/// so they take the server default — measured `utf8mb4_uca1400_ai_ci` on
/// MariaDB 11.8, which is case- AND accent-insensitive. `uq_project_path`
/// therefore holds ONE slot for every ASCII-case spelling of the segment, so a
/// byte-exact comparison here lets `LOCAL` pass the guard, reach the INSERT and
/// occupy the reserved slot — permanently, since `RenameProject` and
/// `ArchiveProject` both answer `UNIMPLEMENTED`. Folding case is what closes
/// that; the door is meant to open one way and this is the half of it a byte
/// comparison left open.
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

    /// **THE RESERVED SEGMENT IS SPELLED OUT HERE AND NOWHERE READ FROM THE
    /// CONSTANT UNDER TEST (ADR-0573).** A test that asserts against
    /// [`RESERVED_ROOT`] moves the day somebody edits [`RESERVED_ROOT`], so it
    /// pins the code to itself rather than to the decision. The literal is the
    /// bound.
    #[test]
    fn the_reserved_root_is_refused_and_the_refusal_names_it() {
        let err = refuse_reserved_root("path", "local")
            .expect_err("the private class root is not an organisation anybody may own");
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains("local"),
            "a refusal must name the value it refused rather than rewrite it (ADR-0569): {}",
            err.message()
        );
    }

    /// **THE RESERVATION IS ABOUT THE SEGMENT, NOT ABOUT THE PREFIX.** Refusing
    /// `local/…` would delete the very class the segment is reserved to carry,
    /// and refusing `locals` would refuse an organisation that merely shares
    /// five characters — the same substring-is-not-a-hierarchy mistake
    /// [`ancestors`] exists to avoid.
    #[test]
    fn a_project_beneath_the_reserved_root_and_a_look_alike_are_both_allowed() {
        for allowed in [
            "local/home/max/src/thing",
            "local/x",
            "locals",
            "local-mirror",
            "local.internal",
            "notlocal",
        ] {
            refuse_reserved_root("path", allowed)
                .unwrap_or_else(|e| panic!("{allowed:?} is not the reserved segment: {e}"));
        }
    }

    /// **THE RESERVED ROOT IS A WELL-FORMED PATH, AND THE GRAMMAR STILL SAYS
    /// SO.** [`validate`] answers "is this a project path"; the reservation
    /// answers "may this path be OWNED". Folding the second into the first would
    /// apply it to every caller of [`validate`] — including `TouchProjects`,
    /// which refuses a whole flush on the first bad path.
    #[test]
    fn the_reserved_root_is_still_a_legal_path_to_the_grammar() {
        validate("path", "local").expect("`local` is a legal path; what it is not is an owner");
    }

    /// **A BYTE-EXACT REFUSAL LEAVES THE DOOR OPEN ONE SHIFT KEY AWAY.**
    /// `project.path` is declared `DEFAULT CHARSET=utf8mb4` with NO `COLLATE`
    /// (`crate::schema`, migration 1), so it takes the server default — measured
    /// `utf8mb4_uca1400_ai_ci` on MariaDB 11.8, which is case-insensitive. So
    /// `uq_project_path` holds ONE slot for every ASCII-case spelling of the
    /// segment, and a registration at `LOCAL` occupies the reserved slot for
    /// ever: `RenameProject` and `ArchiveProject` both answer `UNIMPLEMENTED`, so
    /// nothing can retire it.
    ///
    /// **THE REFUSAL NAMES WHAT THE CALLER SENT, NOT THE CONSTANT.** Echoing
    /// `local` back at somebody who typed `LOCAL` is the quiet rewriting of an
    /// identity that ADR-0569 refuses, and it would tell them their value was
    /// something it was not.
    ///
    /// The spellings are literals rather than derived from [`RESERVED_ROOT`]
    /// (ADR-0573).
    #[test]
    fn every_ascii_case_spelling_of_the_reserved_root_is_refused_and_named() {
        for sent in ["LOCAL", "LoCaL", "Local", "locaL"] {
            let err = refuse_reserved_root("path", sent)
                .expect_err("the collation makes this the same key as `local`");
            assert_eq!(err.code(), tonic::Code::InvalidArgument, "{sent:?}");
            assert!(
                err.message().contains(sent),
                "the refusal must name the value the caller sent rather than the constant \
                 (ADR-0569): {}",
                err.message()
            );
        }
    }

    /// **THE GUARD IS COMPLETE ONLY BECAUSE THIS ALPHABET IS ASCII-ONLY, AND
    /// THIS TEST IS WHAT KEEPS THAT TRUE.** `project.path` collates
    /// `utf8mb4_uca1400_ai_ci` — case-insensitive AND accent-insensitive — so
    /// any two spellings that collate equal share one `uq_project_path` slot.
    /// [`refuse_reserved_root`] answers only the CASE half of that, with
    /// `eq_ignore_ascii_case`. The accent half needs no answer, and the reason is
    /// this set rather than that function: no accented character can appear in a
    /// segment at all, so nothing that would fold onto an ASCII letter ever
    /// reaches the comparison. Within the alphabet below ASCII case is the whole
    /// of what the collation folds — `lo-cal`, `l.ocal` and `lo_cal` all compare
    /// UNEQUAL to `local` on a live MariaDB 11.8, and `LoCaL` compares equal.
    ///
    /// So widening the alphabet by one non-ASCII character reddens this test, and
    /// that is the entire point of it: the reader is sent back to
    /// [`refuse_reserved_root`] to ask what else the collation now folds.
    ///
    /// The same shape as [`an_underscore_is_a_legal_segment_character`], which
    /// exists so that NARROWING the grammar cannot silently retire
    /// `sql::subtree`'s LIKE escaping. The alphabet is written out as a literal
    /// rather than read from the implementation (ADR-0573).
    #[test]
    fn the_segment_alphabet_is_ascii_only_which_is_what_bounds_the_collation_guard() {
        const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-";

        // EVERY Unicode scalar value, not a sample: the claim is that NO
        // non-ASCII character is legal, and a sample cannot make it.
        //
        // The character is embedded after an `x` so that the whole-segment rule —
        // `"."` and `".."` are refused as segments in their own right — does not
        // answer here for the alphabet. That rule is pinned by
        // `the_shapes_that_are_not_paths_are_refused`.
        let mut segment = String::with_capacity(8);
        for code in 0u32..=0x0010_FFFF {
            let Some(c) = char::from_u32(code) else {
                continue;
            };
            segment.clear();
            segment.push('x');
            segment.push(c);
            assert_eq!(
                segment_is_legal(&segment),
                ALPHABET.contains(c),
                "a segment admits exactly {ALPHABET:?} and {c:?} (U+{code:04X}) disagrees. If \
                 the widening is deliberate, re-read `refuse_reserved_root`: the column collates \
                 accent-insensitively, and only an ASCII-only alphabet keeps ASCII case the whole \
                 of what that guard has to fold"
            );
        }

        // THE SAME DOOR, stated through the public function, so that the two
        // cannot drift into meaning different things.
        validate("path", &format!("{R}/a-b_c.d")).expect("the alphabet is legal in a segment");
        validate("path", &format!("{R}/café")).expect_err("a non-ASCII letter is not a segment");
    }

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
