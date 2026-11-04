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
}
