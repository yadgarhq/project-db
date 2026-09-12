# Migration notes

Commands for a human to run, and orderings a human must decide. Nothing here is
applied automatically.

## `actionlint` missing from `$PATH` (ledger 653)

**Nothing to run against the cluster from here.** This is a developer-machine
gap, not a repository change, and it does not touch anything deployed.

**What was checked.** On this machine, `pre-commit` was not installed in this
clone (`.git/hooks/pre-commit` was absent) and `actionlint` was not on `$PATH`
(`which actionlint` found nothing). Installing the hook (`pre-commit install`)
and running `pre-commit run --all-files` surfaced exactly one failure —
`actionlint`, with `Executable \`actionlint\` not found` — and every other hook
(trailing-whitespace, gitleaks, prettier, hadolint, cargo-fmt, cargo-clippy,
cargo-deny, observe-coverage, helm-lint) passed clean. So there is no repo
drift to fix here; the tree is otherwise clean under hooks that had never run
locally.

**This is the fail-closed guard working as intended, not a new defect.**
Ledger 636 (`yadgarhq/actions` v1.12.2, `hooks/actionlint.sh`) replaced a bare
`language: system` entry — which silently disables actionlint's shellcheck
rule class when the binary is absent and still reports `Passed` — with a
script that refuses when `actionlint` is missing (`command -v actionlint`) or
when its shellcheck rule class is not actually running (an SC2086 probe).
Confirmed on this machine: the probe run directly against the nixpkgs
`actionlint` binary already present in the Nix store
(`/nix/store/gsa888rwb1bbicfw87y6bsvbfcxcl073-actionlint-1.7.12/bin/actionlint`)
correctly reports `SC2086` — so that build's wrapper does carry a working
`shellcheck` on its own internal `$PATH`, exactly as `hooks/actionlint.sh`'s
comment claims. **No separate `shellcheck` package is needed** — only
`actionlint` itself has to reach the user's `$PATH`.

**The fix, for the operator to apply.** Add `pkgs.actionlint` to whichever Nix
surface manages this machine's user packages (home-manager `home.packages`, or
the equivalent system package list) — this repository does not manage that
config and the exact insertion point is the operator's to choose. This session
made no edit to any nix config, per standing instruction.

**Verify after applying:**

```
which actionlint
cd project-db && pre-commit run actionlint --all-files
```

Both should succeed — `which actionlint` resolves, and the hook passes rather
than refusing with the missing-executable message.

**The other half of ledger 653 — `pre-commit install` — is a one-time,
per-clone step and not a repository defect.** No repository in this estate
runs it automatically (`grep -ri pre-commit README.md Makefile` in
`project-db`, `iam-db` and `task-db` all come back empty), so every developer
clone needs it once. This session ran `pre-commit install` in this clone to
produce the finding above; that is local state and carries nothing for this
pull request to change.

**One flag for whoever routes this.** Every other `MIGRATION_NOTES.md` in this
estate (`deploy`, `gateway`, `iam`, `estate`) records service deployment
ordering. A developer-machine toolchain gap is a different kind of note. It is
filed here because ledger 653 is scoped to this repository, but `yadgarhq/config`
or `yadgarhq/estate` may be a better home for machine-prerequisite notes that
apply across every clone in the estate — the orchestrator's call, not this
pull request's.

## Migration 1 was CUT FRESH: every existing `project-db` database must be dropped (ledger 881)

**THIS MODULE IS DEPLOYED, AND THE CLAIM THAT IT IS NOT IS STALE.**
`src/schema.rs`'s migration 4 says "this module has no tag, no
`yadgar-deployable` topic and no `argocd/versions` entry, so there is no
populated column anywhere to rebuild". Measured 2026-09-12, all three clauses
are now FALSE: the newest tag is `v0.1.18`, the repository carries the
`yadgar-deployable` topic, and `argocd/versions/project-db.yaml` pins image
`0.1.14` by digest. The repository also holds a `chart/` and a `Containerfile`.
That sentence was true when it was written and is quoted here only to say it
must not be relied on again.

**No cluster command was run to write this note, and none may be**: reading a
deployed database is outside what this session does. So whether the live
instance's `project` table HOLDS ROWS is NOT established here. What follows
therefore applies to the deployed database as well as to developer machines, and
the operator has to decide the ordering.

**What changed.** `src/schema.rs`'s migration 1 (`create_project`) gained three
columns — `source_repo`, `owner_user_id`, `visibility` — two CHECK constraints,
and migration 4's collation pin on `path`, folded forward. Migration 1 was
EDITED rather than a fifth migration appended. That breaks this file's standing
append-never-edit rule, on the one licence the rule admits: nobody uses the
system, so there is no data to carry forward
(`plans/project-validation.md`, given 8).

**The consequence, and it is silent.** A database that already applied migration
1 records version 4 in the ledger and has nothing pending, so it never receives
the new columns. `RegisterProject` against it then fails with
`Unknown column 'source_repo' in 'INSERT INTO'` — an `INTERNAL` with a message
no caller can act on. Nothing detects this at boot.

**IT DESTROYS DATA WHEN IT IS APPLIED TO A POPULATED DATABASE.** The only
recovery from the silent state above is `DROP DATABASE`, which deletes every
registered project — and a project path is a partition key, so every memory,
wiki page, ADR and task stamped with a deleted project's path is orphaned rather
than moved (D52, D53; `RenameProject` is `UNIMPLEMENTED` and the retag job does
not exist). The plan's given 8 says nobody uses the system, not even the
operator, which is what licenses this at all — but that is an operator statement
and not a row count. **Count the rows before dropping anything deployed:**

```
SELECT COUNT(*) FROM project;
```

Zero makes this free. Non-zero is a conversation, not a command: record the
paths first, because nothing in this estate can move a record from one project
to another afterwards.

**What to run, for every database this module has ever migrated:**

```
# The test databases are named by the test that created them, all prefixed
# `project_db_`. List them first, then drop them.
podman exec project-db-test mariadb -uroot -pci \
  -e "SHOW DATABASES LIKE 'project\_db\_%'"
podman exec project-db-test mariadb -uroot -pci \
  -e "DROP DATABASE IF EXISTS <each name listed above>"
```

Simplest and what this session did: throw the container away and start a fresh
one, per the README's own recipe. `World::fresh` drops and recreates its own
database per test, so a fresh container needs nothing else.

**Verify after applying:**

```
export YADGAR_TEST_DSN='mysql://root:ci@127.0.0.1:13306/probe'
cargo test --all-features --test class
```

Eight tests must pass. They perform real INSERTs and assert on the constraint
name the engine reports, so a green run is the engine enforcing rather than the
DDL declaring.
