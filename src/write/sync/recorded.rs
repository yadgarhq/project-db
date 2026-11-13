//! What D67 records for a `SyncProjects` call (yaadgaar #630): a child of
//! `sync` so it calls the same private checks `sync_as` runs.

use super::admitted;
use crate::pb::yadgar::common::v1::Scope;
use crate::sql::tel_scope;

/// The Scope D67 records for a `SyncProjects` call (yaadgaar #630).
///
/// **ONLY AN ADMITTED CALL'S IDENTITY IS RECORDED.** The handler starts its
/// `Call` before [`admitted`] runs, so a refusal is recorded too. On a refused
/// call every identity field in the Scope is an unverified claim: a record
/// carrying it would attribute the refusal to the principal the caller named,
/// the seeder's own in the forgery that matters. So a refused call records
/// `user_id`, `instance_id` and `project_id` empty, and keeps `request_id`,
/// which is D67's join key and not an identity claim.
///
/// **ONE FUNCTION DECIDES BOTH.** [`admitted`] is what
/// [`ProjectDb::sync_as`](crate::service::ProjectDb::sync_as) refuses on: the
/// caller, a Scope, and the principal's width. This helper calls it rather
/// than copying its checks, so what is recorded cannot drift from who is
/// admitted. It decides only what is recorded; `sync_as` still refuses.
///
/// **THE PRINCIPAL CHECK HERE IS SHAPE ONLY** (1 to 64 characters). That the
/// principal is the seeder's is `project`'s check, one hop up (ADR-0942); this
/// hop admits only `project`'s leaf, and records what `project` relayed.
pub(crate) fn recorded_scope(
    caller: Option<&str>,
    scope: &Option<Scope>,
    header: String,
) -> yadgar_telemetry::observe::Scope {
    let claimed = tel_scope(scope, header);
    if admitted(caller, scope.as_ref()).is_ok() {
        return claimed;
    }
    yadgar_telemetry::observe::Scope {
        request_id: claimed.request_id,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::SYNC_CALLER;
    use crate::write::sync::MAX_PRINCIPAL_CHARS;

    /// The seeder's principal, as `project` relays it (ADR-0942).
    const SEEDER: &str = "yadgar:system:seeder";

    /// The `x-yadgar-request-id` a record falls back to when no Scope carries
    /// an id. Distinct from the Scope's, so a record shows which one it took.
    const HEADER: &str = "the-header-s-request-id";

    /// A Scope claiming every identity field, as a refused `SyncProjects` might.
    fn claimed(user_id: &str) -> Option<Scope> {
        Some(Scope {
            request_id: "the-scope-s-request-id".into(),
            user_id: user_id.into(),
            instance_id: "a-claimed-instance".into(),
            project_id: "a/claimed-project".into(),
            ..Default::default()
        })
    }

    /// YAADGAAR #630. An ADMITTED sync records its Scope whole: every check
    /// passed, so the principal is the one `project` relayed.
    ///
    /// **THE MUTATION THAT REDS IT:** blank the identity fields on every call.
    #[test]
    fn an_admitted_sync_records_its_scope_whole() {
        let tel = recorded_scope(Some(SYNC_CALLER), &claimed(SEEDER), HEADER.into());
        assert_eq!(tel.request_id, "the-scope-s-request-id");
        assert_eq!(tel.user_id, SEEDER);
        assert_eq!(tel.instance_id, "a-claimed-instance");
        assert_eq!(tel.project_id, "a/claimed-project");
    }

    /// YAADGAAR #630. A REFUSED sync records the `request_id`, D67's join key,
    /// and no identity claim. Each case fails exactly one check, so a helper
    /// that skipped one of them records a claim on that case.
    ///
    /// **THE MUTATIONS THAT RED IT:** record the claimed Scope; check only the
    /// caller; check only the principal; blank `request_id`; keep
    /// `instance_id`; keep `project_id`.
    #[test]
    fn a_refused_sync_records_only_its_request_id() {
        let too_wide = "p".repeat(MAX_PRINCIPAL_CHARS + 1);
        let cases = [
            ("another leaf", Some("gateway-caller"), claimed(SEEDER)),
            ("no caller", None, claimed(SEEDER)),
            ("an empty principal", Some(SYNC_CALLER), claimed("")),
            (
                "a principal too wide",
                Some(SYNC_CALLER),
                claimed(&too_wide),
            ),
            ("no Scope", Some(SYNC_CALLER), None),
        ];
        for (case, caller, scope) in cases {
            let want = if scope.is_some() {
                "the-scope-s-request-id"
            } else {
                HEADER
            };
            let tel = recorded_scope(caller, &scope, HEADER.into());
            assert_eq!(tel.request_id, want, "{case}");
            assert_eq!(tel.user_id, "", "{case}");
            assert_eq!(tel.instance_id, "", "{case}");
            assert_eq!(tel.project_id, "", "{case}");
        }
    }

    /// A Scope with a given `request_id` and the seeder's principal.
    fn with_request_id(request_id: String) -> Option<Scope> {
        Some(Scope {
            request_id,
            user_id: SEEDER.into(),
            ..Default::default()
        })
    }

    /// `n` copies of `c`, then the cut marker: a capped request id.
    fn cut(c: char, n: usize) -> String {
        format!("{}...", c.to_string().repeat(n))
    }

    /// AC1: an admitted call's Scope `request_id` is capped; identity kept.
    #[test]
    fn request_id_cap_admitted_scope_id_is_cut() {
        // AC1: main, 1000 chars -> first 256 plus "..." (259), user_id kept
        let scope = with_request_id("z".repeat(1000));
        let tel = recorded_scope(Some(SYNC_CALLER), &scope, "h".into());
        assert_eq!(tel.request_id, cut('z', 256));
        assert_eq!(tel.request_id.chars().count(), 259);
        assert_eq!(tel.user_id, SEEDER);
        // AC1: triangulation, other char and length
        let scope = with_request_id("q".repeat(500));
        let tel = recorded_scope(Some(SYNC_CALLER), &scope, "h".into());
        assert_eq!(tel.request_id, cut('q', 256));
    }

    /// AC1: a refused call still records a capped `request_id`, no identity.
    #[test]
    fn request_id_cap_refused_scope_id_is_cut() {
        // AC1: refusal, caller None, 1000 chars
        let scope = with_request_id("z".repeat(1000));
        let tel = recorded_scope(None, &scope, "h".into());
        assert_eq!(tel.request_id, cut('z', 256));
        assert_eq!(tel.user_id, "");
        // AC1: triangulation, another leaf as caller
        let scope = with_request_id("w".repeat(700));
        let tel = recorded_scope(Some("gateway-caller"), &scope, "h".into());
        assert_eq!(tel.request_id, cut('w', 256));
        assert_eq!(tel.user_id, "");
    }

    /// AC1: with no Scope the header fallback is capped.
    #[test]
    fn request_id_cap_header_fallback_is_cut() {
        // AC1: main, no Scope, 1000-char header
        let tel = recorded_scope(Some(SYNC_CALLER), &None, "y".repeat(1000));
        assert_eq!(tel.request_id, cut('y', 256));
        // AC1: triangulation, other char and length, no caller
        let tel = recorded_scope(None, &None, "k".repeat(400));
        assert_eq!(tel.request_id, cut('k', 256));
    }

    /// AC1: 257 characters is cut to 256 plus the marker.
    #[test]
    fn request_id_cap_boundary_257_is_cut() {
        // AC1: boundary, 257 chars
        let scope = with_request_id("a".repeat(257));
        let tel = recorded_scope(None, &scope, "h".into());
        assert_eq!(tel.request_id, cut('a', 256));
        // AC1: triangulation, 300 chars
        let scope = with_request_id("b".repeat(300));
        let tel = recorded_scope(None, &scope, "h".into());
        assert_eq!(tel.request_id, cut('b', 256));
    }

    /// AC1: the cut counts characters, not bytes, so the result is valid UTF-8.
    #[test]
    fn request_id_cap_counts_characters_not_bytes() {
        let e_acute = char::from_u32(0xE9).unwrap();
        // AC1: boundary, 300 two-byte chars
        let scope = with_request_id(e_acute.to_string().repeat(300));
        let tel = recorded_scope(None, &scope, "h".into());
        assert_eq!(tel.request_id, cut(e_acute, 256));
        assert_eq!(tel.request_id.chars().count(), 259);
        // AC1: triangulation, three-byte char, 400 of them
        let euro = char::from_u32(0x20AC).unwrap();
        let scope = with_request_id(euro.to_string().repeat(400));
        let tel = recorded_scope(None, &scope, "h".into());
        assert_eq!(tel.request_id, cut(euro, 256));
    }

    /// AC1 property: capping a capped value changes nothing.
    #[test]
    fn request_id_cap_is_idempotent() {
        for (c, n) in [('z', 1000_usize), ('a', 257), ('\u{E9}', 300)] {
            // AC1: idempotent, scope source
            let scope = with_request_id(c.to_string().repeat(n));
            let once = recorded_scope(None, &scope, "h".into()).request_id;
            assert_eq!(once, cut(c, 256));
            let again = recorded_scope(None, &with_request_id(once.clone()), "h".into());
            assert_eq!(again.request_id, once);
            // AC1: idempotent, header source
            let once = recorded_scope(None, &None, c.to_string().repeat(n)).request_id;
            let again = recorded_scope(None, &None, once.clone());
            assert_eq!(again.request_id, once);
        }
    }
}
