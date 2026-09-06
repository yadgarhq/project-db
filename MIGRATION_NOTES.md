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
