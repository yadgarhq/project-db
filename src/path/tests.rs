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

/// [`is_private_class`] answers the same question `ck_project_class_path`
/// asks, and the two must never disagree.
///
/// **THE LITERALS ARE SPELLED OUT RATHER THAN BUILT FROM
/// [`RESERVED_ROOT`] (ADR-0573)**, for the reason the test above gives: a
/// case built from the constant under test moves when the constant moves.
///
/// The membership table mirrors `LIKE 'local/%'` under
/// `utf8mb4_general_ci`, row for row. The BARE segment is OUT, in both
/// rules — `local` does not match `local/%` — and a look-alike prefix is out
/// because a segment is not a substring.
#[test]
fn the_private_class_is_the_segment_beneath_the_reserved_root_in_any_case() {
    for inside in [
        "local/jaguar/thing",
        "LOCAL/jaguar/thing",
        "LoCaL/x",
        "local/x",
        "local/",
    ] {
        assert!(
            is_private_class(inside),
            "{inside:?} is the private class and `LIKE 'local/%'` agrees"
        );
    }
    for outside in [
        "local",
        "LOCAL",
        "locals",
        "locals/x",
        "local-mirror/x",
        "local.internal/x",
        "notlocal/x",
        "pangolin/local/x",
        "",
    ] {
        assert!(
            !is_private_class(outside),
            "{outside:?} is not the private class and `LIKE 'local/%'` agrees"
        );
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
/// `project.path` collates `utf8mb4_general_ci` (`crate::schema`,
/// migration 4), which is case-insensitive. So
/// `uq_project_path` holds ONE slot for every ASCII-case spelling of the
/// segment, and a registration at `LOCAL` occupies the reserved slot for
/// ever: `crate::write` holds both `RenameProject` and `ArchiveProject` at
/// `UNIMPLEMENTED` in this release, so nothing can retire it.
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
/// `utf8mb4_general_ci` — case-insensitive AND accent-insensitive — so
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
