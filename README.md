# project-db

The `project` module's **`-db` twin**: the registry of projects, and the only
writer of its store, serving `ProjectDbService` over gRPC. It holds no business
rules — the boundary is the job.

## Why this exists

Every module in the estate scopes its rows on a project id, and nothing
validates that the id names a real project. A typo silently mints a phantom
scope and the symptom is "my tasks vanished". The registry is a **base
requirement**, not a feature of `task`: `ask`, `recall` and the audit store each
need the same key, which is why it is its own twin rather than a check inside a
module that owns something else (D7, D54).

Decisions in [`yadgarhq/docs`](https://github.com/yadgarhq/docs): D52 (`project`
is its own module, and an unregistered project is never auto-created), D53
(project ids are hierarchical paths, and the hierarchy has meaning), D4 (the twin
as connection concentrator), D5 (one call, one transaction), D7 (capabilities,
not SQL dialects), D69 (the probe), D70 (how the protos get here), ADR-0227 (an
identity is never derived).

## The three rules the contract's own comments oblige

**A path is canonical and hierarchical.** `Project.path` is the value that lands
in every other entity's `Meta.project_id`. The hierarchy is SEMANTIC, so
resolution walks it **segment by segment**: a registered `acme/team` is not an
ancestor of `acme/teamx/svc`, however much of its text they share.

**A rename is an alias, never a rewrite.** Moving a project does not touch a
single record carrying the old path. A former path resolves through
`project_alias`, and so does every path beneath it — otherwise one rename would
strand a whole subtree. The store holds aliases and every read path follows one,
which is why those paths exist before the verb that creates one does.

**`last_seen_at` is debounced.** `TouchProjects` is a periodic flush from a
Valkey bucket, never a per-request write. It advances the column
**monotonically**, and it moves neither `version` nor `updated_at`: the first is
D8's compare-and-set counter and the second means "when did this REGISTRATION
last change".

## `RenameProject` and `ArchiveProject` answer `UNIMPLEMENTED`

**Held back on a data argument, not an effort one.** An alias keeps a stored
`Meta.project_id` valid; it does not keep it CURRENT. Every memory, wiki page,
ADR and task already stamped with a project path goes on carrying it, and the job
that retags them does not exist — so a rename shipped today silently orphans
those records rather than moving them: readable, valid, and absent from every
view of the project they belong to. Archiving has the same shape one step
earlier, since an instance must be able to retag before anything is archived.
Both verbs return together with that retag job.

**Refused, never omitted.** A method a service silently lacks answers
`UNIMPLEMENTED` from tonic's own fallback with no reason in it, and a caller
cannot tell that from a version skew or a bad route. These refuse with the reason
written out, which is the estate's existing shape. The refusal is the first thing
either handler does, before any transaction — so a held-back verb never spends
the idempotency key it was handed.

`RegisterProject` is implemented and **has no caller in this release**:
organisation-level projects are defined by GitOps rather than created on demand
(D43, D52).

## Nothing is ever auto-created

`ResolveProject` answers an unregistered path with its **nearest registered
ancestor** rather than an error, so a typo lands in a real parent and a new
service directory works before anyone registers it (D52 as amended by D53). The
softness is reported rather than silent: `exact` is false, and the caller
surfaces that as a D39 notice naming the id to register. Only when NO ancestor is
registered does it refuse.

Registration is an **administrative** act — GitOps or CLI, like configuration
(D43). Agents resolve and read; they never mint a partition key.

## `Scope` does not filter a row here

Every other `-db` in this estate turns `Scope` into a `WHERE` clause. This one
cannot: `ResolveProject` is the caller asking _which_ project a candidate path
belongs to, and `Scope.project_id` is derived from that same candidate — so
filtering by it could only return what the caller already assumed, and the
ancestor walk D52 requires would be unreachable. `ListProjects` carries its own
subtree parameter, `under_path`, and `Project` carries no visibility, team or
owner for a per-row decision to be made against.

**So the registry is readable deployment-wide, and that is stated rather than
left to be inferred from code that happens not to filter.** One deployment is one
organisation (D27), and what this store holds is the set of NAMES that
organisation registered — not the content filed under them. A caller that can
reach this service can list every project path in the installation. If that is
ever wrong it is wrong at the level of who may reach the service.

`Scope` is still required on every rpc and refused when absent: it carries
`request_id`, without which D67's telemetry cannot be summed across hops, and
`user_id` and `project_id`, which key D9's idempotency ledger.

## The protos are vendored, not fetched

`proto/` is a **subset** of [`yadgarhq/proto`](https://github.com/yadgarhq/proto),
exported at the tag in `PROTO_VERSION` for the packages in `PROTO_PATHS`. Buf
closes the import graph itself, so listing `yadgar/project/v1` also brings
`yadgar/common/v1`.

```bash
make proto      # refresh from the pin — the only sanctioned way to change proto/
```

CI re-runs that export and **fails on any difference**. Vendoring is normally skew
waiting to happen and is defensible here only because that check exists.

## Boot order is a decision, not wiring

Probe → migrate → serve, and the process does not listen until all three succeed.

A capability gap is a boot failure (D7), and the probe runs before the pool is
declared ready (D69) so a failure is a crash-loop rather than a pod that accepts
traffic and fails queries. This module requires `transactions` and `row-locking`
— **not** vector or full-text. A registry is looked up by path and by prefix
(D10).

## No timestamp crosses this boundary as a timestamp

`sqlx` here carries neither `chrono` nor `time`, so no Rust type decodes a
MariaDB `TIMESTAMP` and a statement selecting one fails at DECODE rather than at
compile time. Every timestamp is converted by the ENGINE —
`CAST(UNIX_TIMESTAMP(col) AS SIGNED)` into an `Option<i64>` — and assembled into
`google.protobuf.Timestamp` in `src/rows.rs`. The cast lives in one column list
so no statement can forget it.

## Local development

```bash
podman run -d --rm --name project-db-test -e MARIADB_ROOT_PASSWORD=ci \
  -p 13306:3306 docker.io/library/mariadb:11.8
podman exec project-db-test mariadb -uroot -pci -e "CREATE DATABASE probe"
export YADGAR_TEST_DSN='mysql://root:ci@127.0.0.1:13306/probe'
cargo test
```

The DSN **must end in a database that already exists** — the fixture finds the
server by splitting on the first slash after the scheme, then connects to the
named database in order to create its own.

`protoc` must be on `PATH` — types are generated from the contract, never
hand-written (D16). The `rust-build` base image carries it; on NixOS,
`nix-shell -p protobuf`.

The tests **panic** rather than skip without `YADGAR_TEST_DSN`. A contract suite
that quietly passes with no engine behind it proves nothing.

## Configuration

There are **no compiled-in defaults behind the rotation schedule** (ADR-0569): it
is read from the `shared` ConfigMap `yadgarhq/config` renders, mounted as a
DIRECTORY at `/etc/yadgar/config/shared`, and an absent or half-written document
refuses the boot naming the file.

| variable                                                        | default                                      |                                                                                                                                                                                    |
| --------------------------------------------------------------- | -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `DB_HOST` / `DB_PORT` / `DB_NAME` / `DB_USER`                   | `127.0.0.1` / `3306` / `project` / `project` |                                                                                                                                                                                    |
| `DB_PASSWORD_FILE`                                              | `/var/run/secrets/project-db/password`       | a mounted Secret the operator issued (D58) — never an env var                                                                                                                      |
| `DB_MAX_CONNECTIONS` / `REPLICAS` / `DB_ENGINE_MAX_CONNECTIONS` | `8` / `2` / `151`                            | the product is checked at boot and refused if it would exhaust the engine (D4)                                                                                                     |
| `DB_SSL_MODE`                                                   | `required`                                   | how TLS is negotiated to the engine, for BOTH D7's boot probe and the serving pool: `disabled`, `preferred`, `required`, `verify_identity`. An unrecognised value refuses the boot |
| `DB_SSL_CA_FILE`                                                | unset                                        | the authority `verify_identity` checks the engine against. Setting it WIDENS the trust set rather than replacing it — sqlx seeds the public web roots first                        |
| `LISTEN`                                                        | `0.0.0.0:50051`                              | `LISTEN_TLS_ENABLED=1` with `LISTEN_TLS_CERT_FILE` and `LISTEN_TLS_KEY_FILE` serves over TLS. Asking for TLS without the files refuses the boot rather than downgrading            |
| `METRICS_LISTEN`                                                | `0.0.0.0:9090`                               |                                                                                                                                                                                    |

There is deliberately **no `DB_REQUIRE_TLS` refusal** here, unlike in `task-db`
and `iam-db`. That refusal exists in those modules because they once READ the
key; this one never has, and a deprecation notice for a history a module does not
have is noise.

## What is not built yet

**`RenameProject` and `ArchiveProject`**, above, and the retag job they wait on.

**A depth cap.** D53 says the depth of a project path is capped and that the
limit is CONFIGURATION (D43). ADR-0569 leaves a configuration knob no
compiled-in default to fall back on, so a constant invented here would be the
defect rather than a placeholder for the fix — the cap needs a line in
`yadgarhq/config`'s `project-db.yaml`, which this module ships empty. What bounds
a path today is its LENGTH, at the width of the column that stores it — and the
paths this store must accept are DEEP, because the private class of project ids
is keyed on the full directory path rather than on its basename
(`local/<basename>` collides between two unrelated directories of the same name).
The client trims for length, never for depth.

## `local` is reserved, and only the bare segment

`local` is the first segment of the PRIVATE class of project ids above, so it is
refused as a project of its own: `RegisterProject("local")` answers
`INVALID_ARGUMENT` naming the value, and `ResolveProject` drops the segment from
the ancestor walk. `local/<anything>` stays registrable — that is the class the
reservation exists to carry, and the check is equality on the segment rather than
a prefix match, so `locals` is an ordinary organisation.

That equality folds ASCII case, and the STORE is what decides it. `project.path`
and `project_alias.alias_path` collate `utf8mb4_general_ci`, which is case- and
accent-insensitive, and migration 4 DECLARES it: migrations 1 and 2 named no
`COLLATE`, so until then the columns took whatever `@@collation_server` was and
this guard rested on a setting no deployment states. `uq_project_path` therefore
holds one slot for every ASCII-case spelling, so a byte-exact refusal would let
`LOCAL` pass the guard and occupy the reserved slot for ever. ASCII case is the
whole of what the collation folds here, and that is a property of the GRAMMAR
rather than of the guard: a segment admits only `[A-Za-z0-9._-]`, so no accented
character ever reaches the comparison, and within that alphabet `lo-cal`,
`l.ocal` and `lo_cal` all compare unequal to `local` on the engine. The refusal
names the spelling the caller sent rather than the constant.

Both halves are needed and neither is redundant. Without the write-side refusal a
project registered at `local` becomes the nearest registered ancestor of every
private path in the estate, and each of them resolves into it with `exact: false`
— one stranger's partition key stamped on all of them. Without the read-side
filter that stays true for a row an earlier build already accepted, because a
code guard does not delete rows; the filter closes the walk against the store as
it is, which is why no migration is required to make this safe.

The refusal ships ahead of the first caller rather than behind it because the
door only opens one way: a registration here is immutable, `RenameProject` is
held back, and a `local` once claimed could never be renamed away.

## Every path comparison in Rust folds ASCII case, not only the reserved one

The reservation above is one instance of a rule that binds this whole crate: a
path comparison written in Rust is weaker than the unique index guarding it
unless it folds ASCII case, because the engine folds it and the index therefore
holds one slot per spelling. Two comparisons were weaker than their index and
both are closed.

`ResolveProject` reports `exact` by comparing the row the engine found against
the candidate the caller sent. Byte-wise, `ResolveProject("ACME/ALPHA")` against
a registered `acme/alpha` answered `resolved_path: "acme/alpha"` with `exact:
false` — a soft failure reported on a row that matched EXACTLY. That answer is a
dead end rather than a cosmetic defect: a caller surfaces `exact: false` as a D39
notice naming the id to register, and `RegisterProject` then answers
`ALREADY_EXISTS` on the very index that matched. `via_alias` is gated on `exact`,
so the alias arm gave the same wrong answer.

`TouchProjects` deduplicates its batch before counting how many of its paths
resolve to a registered project, and the count it compares against comes from the
engine. A byte-wise dedup left `alpha` and `ALPHA` in the batch as two while
`COUNT(*)` found the one row they both name, so a flush in which everything
matched logged the warning that says nothing did. Log-only, and a warning that
cries wolf is one nobody reads.

Folding ASCII case is exactly as wide as the engine and no wider, for the reason
the section above gives: within the grammar `validate` admits, ASCII case is the
whole of what this collation folds. Migration 4 is what makes the engine's half of
that a declaration rather than a server setting; neither Rust-side fix creates,
deletes or rewrites a row.

## The Service is headless, deliberately

A normal Service balances at L4, and a gRPC client holds one long-lived HTTP/2
connection — so it would pin to a single pod and leave the rest idle.
`clusterIP: None` publishes every pod address and the client balances across them
(D23).
