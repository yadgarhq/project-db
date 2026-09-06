//! `ResolveProject`, `GetProject` and `ListProjects`.

use tonic::Status;

use crate::path;
use crate::pb::yadgar::project::v1::*;
use crate::rows::{row_to_project, COLUMNS};
use crate::service::ProjectDb;
use crate::sql::{holes, internal, scope_of, subtree, ESCAPE};

/// D56 bounds reads: an unbounded page is how one caller takes the whole table.
const MAX_PAGE: i32 = 500;
const DEFAULT_PAGE: i32 = 50;

impl ProjectDb {
    /// THE load-bearing rpc. Every scoped write in the estate resolves its
    /// candidate path here before it is stamped on a record.
    ///
    /// **NOTHING IS EVER CREATED, and that is the whole of D52.** Auto-creation
    /// on first write is how a typo becomes a permanent scope: one transposed
    /// character mints a project, writes land in it, and nothing ever surfaces
    /// that the corpus has been split in two. No `INSERT` is reachable from this
    /// function.
    ///
    /// **AN UNREGISTERED PATH RESOLVES TO ITS NEAREST REGISTERED ANCESTOR
    /// RATHER THAN FAILING** (D52 as amended by D53), so a typo lands in a real
    /// parent and a genuinely new service directory works before anyone
    /// registers it. The soft failure is made VISIBLE rather than silent:
    /// `exact` is false, and the caller surfaces that as a D39 notice naming the
    /// id to register. Only when NO ancestor is registered does this refuse.
    ///
    /// **AN ANCESTOR IS FOUND THROUGH AN ALIAS TOO.** A rename is an alias and
    /// never a rewrite (D53), so renaming `<root>/wfm` leaves every descendant
    /// path in every record still naming the old parent. If the walk consulted
    /// live paths only, one rename would strand the entire subtree beneath it —
    /// which is precisely the rewrite the alias exists to avoid, arriving as a
    /// resolution failure instead of as a migration.
    pub(crate) async fn resolve(
        &self,
        req: ResolveProjectRequest,
    ) -> Result<ResolveProjectResponse, Status> {
        let _scope = scope_of(&req.scope)?;
        path::validate("candidate_path", &req.candidate_path)?;
        // BEFORE `ancestors`, NOT AFTER, and the ordering is load-bearing rather
        // than tidy. The reserved segment is dropped from the chain below, so a
        // candidate that IS the bare segment would filter down to an empty chain
        // — and `holes(0)` renders `IN ()`, which is a syntax error the caller
        // would receive as `INTERNAL "storage error"`. Refusing it here answers
        // the same question with the reason in it.
        path::refuse_reserved_root("candidate_path", &req.candidate_path)?;

        // THE RESERVED SEGMENT IS NOT AN ANCESTOR OF ANYTHING, EVEN IF A ROW
        // SITS ON IT. `register` refuses to mint one now, and a code-only guard
        // does not remove a row an earlier build already accepted — so the walk
        // is what has to be closed, not only the write. Dropping the segment
        // from the chain covers BOTH arms of the union below, because the same
        // list is bound twice: an alias at `local` is neutralised by the same
        // line as a live path at `local`, with no second guard to keep in step.
        //
        // ASCII CASE IS FOLDED HERE FOR THE SAME REASON THE REFUSAL ABOVE FOLDS
        // IT, and this comparison must stay THE SAME comparison as that one. The
        // walk is evaluated by the ENGINE, and `p.path IN (…)` compares under the
        // column's collation — `utf8mb4_general_ci`, which `crate::schema`
        // declares in migration 4 — so a chain still carrying `LOCAL` matches a row
        // spelled `local`. A byte comparison here drops nothing from such a chain
        // and the squatted row is found anyway.
        //
        // The chain can never be emptied by this, and that invariant rests on the
        // two comparisons being identical: the bare segment IN ANY SPELLING is
        // refused above, so a surviving candidate is either deeper than one
        // segment — leaving at least its own entry — or a single segment that
        // does not fold onto the reserved one. Make one of the two byte-exact and
        // the other not, and `holes(0)` renders `IN ()`, which the caller
        // receives as `INTERNAL "storage error"`.
        let chain: Vec<&str> = path::ancestors(&req.candidate_path)
            .into_iter()
            .filter(|ancestor| !ancestor.eq_ignore_ascii_case(path::RESERVED_ROOT))
            .collect();
        let holes = holes(chain.len());

        // ONE ROUND TRIP FOR THE WHOLE WALK. A loop that queries per level costs
        // a round trip per path segment on the hottest rpc in the estate, and
        // its levels are not independent — a concurrent rename between two of
        // them would let the walk step over a live parent. One statement sees
        // one snapshot.
        //
        // `matched` is what was found, `resolved` is the live path it names, and
        // the two differ exactly when an alias was followed.
        //
        // AUDIT: the interpolations are this module's own column list and a
        // count of `?` placeholders; every caller value is a bound parameter.
        let sql = format!(
            "SELECT p.path AS matched, p.path AS resolved, p.status AS status,
                    CAST(0 AS SIGNED) AS via_alias
               FROM project p
              WHERE p.path IN ({holes})
              UNION ALL
             SELECT a.alias_path AS matched, p.path AS resolved, p.status AS status,
                    CAST(1 AS SIGNED) AS via_alias
               FROM project_alias a
               JOIN project p ON p.id = a.project_id
              WHERE a.alias_path IN ({holes})"
        );
        let mut query = sqlx::query_as::<_, (String, String, i8, i64)>(sqlx::AssertSqlSafe(sql));
        for candidate in chain.iter().chain(chain.iter()) {
            query = query.bind(*candidate);
        }
        let found = query.fetch_all(&self.pool).await.map_err(internal)?;

        // THE DEEPEST MATCH WINS, and the ancestor chain is a chain of strict
        // prefixes, so the longest `matched` IS the deepest. A live path beats
        // an alias at the same depth: the write path refuses a registration that
        // collides with an alias, so the tie should be unreachable, and a
        // deterministic answer is what stops an unreachable case from becoming a
        // coin-flip if it ever becomes reachable.
        let best = found
            .iter()
            .max_by_key(|(matched, _, _, via_alias)| (matched.len(), -*via_alias))
            .ok_or_else(|| {
                Status::not_found(format!(
                    "no registered project matches {:?} or any ancestor of it. Nothing is ever \
                     auto-created here: registration is an administrative act, through GitOps or \
                     the CLI, and never an agent operation (D52). Register the path, or a parent \
                     of it, and retry",
                    req.candidate_path
                ))
            })?;

        let (matched, resolved, status, via_alias) = best;
        // ASCII CASE IS FOLDED HERE FOR THE THIRD TIME IN THIS RESOLUTION, AND
        // ALL THREE MUST STAY THE SAME COMPARISON — the reserved-segment refusal
        // above, the chain filter above, and this. `matched` is a value the
        // ENGINE selected, under `utf8mb4_general_ci` (`crate::schema`
        // declares it in migration 4), so the row it names may be spelled in a
        // different ASCII case than the candidate that found it. A byte
        // comparison then answers `exact: false` about a row the engine matched
        // EXACTLY.
        //
        // THAT WRONG ANSWER IS A DEAD END RATHER THAN A COSMETIC DEFECT. The
        // caller surfaces `exact: false` as a D39 notice naming the id to
        // register (D52), and `RegisterProject` answers `ALREADY_EXISTS` on the
        // same `uq_project_path` that just matched — so the notice instructs an
        // operator to perform the one action the store refuses, for ever.
        //
        // The fold is exactly as wide as the engine's and no wider: `validate`
        // admits only `[A-Za-z0-9._-]`, and within that alphabet ASCII case is
        // the whole of what this collation folds (measured — `-`, `.` and `_`
        // each compare equal to nothing but themselves), and migration 4 is what
        // makes that collation the schema's rather than the server's.
        let exact = matched.eq_ignore_ascii_case(&req.candidate_path);
        Ok(ResolveProjectResponse {
            resolved_path: resolved.clone(),
            exact,
            // The contract scopes this to the CANDIDATE — "set when
            // candidate_path matched an alias rather than a live path" — so an
            // ANCESTOR reached through an alias does not set it. When `exact` is
            // false the candidate matched nothing at all, and reporting that it
            // matched an alias would be a statement about a different path than
            // the one the caller sent.
            via_alias: exact && *via_alias == 1,
            status: *status as i32,
        })
    }

    /// One project, by its canonical path or by a former one.
    ///
    /// **AN ALIAS RESOLVES HERE; AN ANCESTOR DOES NOT.** That is the whole
    /// difference between this rpc and [`ProjectDb::resolve`], and it is a line
    /// worth being able to state: `Get` answers "what is the project this path
    /// names", and a former path still names it (D53) — every record written
    /// before a rename carries the old path, so a consumer reading one of those
    /// would otherwise need two calls to display the project it belongs to. An
    /// ANCESTOR names a DIFFERENT project, so answering with it would be a
    /// substitution rather than a lookup; that judgement belongs to `Resolve`,
    /// which reports it in `exact`.
    ///
    /// The response carries the canonical `path` and every alias, so a caller
    /// that arrived by an old path can see which one it reached.
    pub(crate) async fn get(&self, req: GetProjectRequest) -> Result<GetProjectResponse, Status> {
        let _scope = scope_of(&req.scope)?;
        path::validate("path", &req.path)?;

        // AUDIT: the interpolation is this module's own column list; every
        // caller value is a bound parameter.
        let sql = format!(
            "SELECT {COLUMNS} FROM project
              WHERE path = ?
                 OR id = (SELECT project_id FROM project_alias WHERE alias_path = ?)
              ORDER BY CASE WHEN path = ? THEN 0 ELSE 1 END
              LIMIT 1"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&req.path)
            .bind(&req.path)
            .bind(&req.path)
            .fetch_optional(&self.pool)
            .await
            .map_err(internal)?
            .ok_or_else(|| {
                Status::not_found(format!(
                    "no project is registered at {:?}, and it is not a former path of one",
                    req.path
                ))
            })?;

        let project = self.with_aliases(vec![row_to_project(&row)?]).await?;
        Ok(GetProjectResponse {
            project: project.into_iter().next(),
        })
    }

    pub(crate) async fn list(
        &self,
        req: ListProjectsRequest,
    ) -> Result<ListProjectsResponse, Status> {
        let _scope = scope_of(&req.scope)?;
        let limit = page_size(req.page_size);
        let status = status_filter(req.status)?;

        // AUDIT: the interpolations are this module's own column list and
        // predicate; every caller value is a bound parameter.
        let mut sql = format!("SELECT {COLUMNS} FROM project WHERE 1 = 1");
        if !req.under_path.is_empty() {
            path::validate("under_path", &req.under_path)?;
            // The subtree INCLUDES the named project itself, which is the
            // reading D53 gives: a query at `<root>/wfm` sees `<root>/wfm` and
            // every descendant. The equality arm is what makes that true — the
            // LIKE pattern alone matches only what is strictly beneath.
            sql.push_str(&format!(" AND (path = ? OR path LIKE ? {ESCAPE})"));
        }
        if status.is_some() {
            sql.push_str(" AND status = ?");
        }
        if !req.page_token.is_empty() {
            // Keyset, not OFFSET. `path` is UNIQUE and is the sort key, so a
            // page boundary is a value rather than a count — which is what keeps
            // a row from being skipped or repeated when the registry changes
            // between pages.
            sql.push_str(" AND path > ?");
        }
        // One more than asked for, purely to learn whether a next page exists.
        // Returning a token unconditionally would read as "there is more" on the
        // final page and cost the caller a round trip to find out otherwise.
        sql.push_str(" ORDER BY path LIMIT ?");

        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
        if !req.under_path.is_empty() {
            query = query.bind(&req.under_path).bind(subtree(&req.under_path));
        }
        if let Some(status) = status {
            query = query.bind(status as i8);
        }
        if !req.page_token.is_empty() {
            query = query.bind(&req.page_token);
        }
        let mut rows = query
            .bind(i64::from(limit) + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;

        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let projects = rows
            .iter()
            .map(row_to_project)
            .collect::<Result<Vec<_>, _>>()?;
        let projects = self.with_aliases(projects).await?;

        let next_page_token = match projects.last() {
            Some(last) if more => last.path.clone(),
            _ => String::new(),
        };
        Ok(ListProjectsResponse {
            projects,
            next_page_token,
        })
    }

    /// Fill in `Project.aliases` for a whole page, in ONE statement.
    ///
    /// **THIS EXISTS BECAUSE THE ALTERNATIVE HAS ALREADY SHIPPED IN THIS ESTATE
    /// ONCE.** `task-db`'s migration 2 records it: the contract carried `tags`
    /// and `links` from its first tag, the store had neither, "so `row_to_task`
    /// returned an empty vec for each and a caller's tags vanished without a
    /// word". A repeated field that no read path fills is invisible — it decodes
    /// as empty, which is a legal value, so nothing anywhere reports a problem.
    /// Every path that returns a `Project` goes through here.
    ///
    /// A second statement rather than a join or a `GROUP_CONCAT`: a join
    /// multiplies the page by the alias count and `row_to_project` would then
    /// have to de-duplicate it, and `GROUP_CONCAT` invents a separator that a
    /// path could contain and silently truncates at `group_concat_max_len`.
    async fn with_aliases(&self, mut projects: Vec<Project>) -> Result<Vec<Project>, Status> {
        if projects.is_empty() {
            return Ok(projects);
        }
        let ids: Vec<String> = projects
            .iter()
            .filter_map(|p| p.meta.as_ref().map(|m| m.id.clone()))
            .collect();
        if ids.is_empty() {
            return Ok(projects);
        }

        // AUDIT: the interpolation is a count of `?` placeholders; every value
        // is bound. ORDER BY so that a page renders the same twice — an
        // unordered repeated field makes an otherwise identical response differ
        // between calls.
        let sql = format!(
            "SELECT project_id, alias_path FROM project_alias
              WHERE project_id IN ({}) ORDER BY alias_path",
            holes(ids.len())
        );
        let mut query = sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(sql));
        for id in &ids {
            query = query.bind(id);
        }
        let pairs = query.fetch_all(&self.pool).await.map_err(internal)?;

        for project in &mut projects {
            let Some(id) = project.meta.as_ref().map(|m| m.id.as_str()) else {
                continue;
            };
            project.aliases = pairs
                .iter()
                .filter(|(project_id, _)| project_id == id)
                .map(|(_, alias)| alias.clone())
                .collect();
        }
        Ok(projects)
    }
}

/// A page size the caller did not set is 0, which would return nothing and look
/// like an empty registry. Bounded above as well (D56).
fn page_size(requested: i32) -> i32 {
    match requested {
        n if n <= 0 => DEFAULT_PAGE,
        n if n > MAX_PAGE => MAX_PAGE,
        n => n,
    }
}

/// ABSENT means every status; PRESENT-AND-UNSPECIFIED is refused.
///
/// The field is `optional`, so proto3 gives it explicit presence and the two are
/// distinguishable on the wire — which is what makes refusing the second
/// possible at all. A caller that deliberately sent `PROJECT_STATUS_UNSPECIFIED`
/// asked for something, and answering it with "every status" would silently
/// widen a filter rather than report that it named nothing. Same reasoning
/// `ListTasks` gives for refusing an unrecognised `TaskStatus`: a page that
/// answers a question the caller did not ask still looks authoritative.
fn status_filter(requested: Option<i32>) -> Result<Option<ProjectStatus>, Status> {
    let Some(value) = requested else {
        return Ok(None);
    };
    match ProjectStatus::try_from(value) {
        Ok(ProjectStatus::Unspecified) => Err(Status::invalid_argument(
            "status was sent as PROJECT_STATUS_UNSPECIFIED, which names no status. Omit the \
             field to list every status",
        )),
        Ok(status) => Ok(Some(status)),
        Err(_) => Err(Status::invalid_argument(format!(
            "{value} is not a ProjectStatus"
        ))),
    }
}
