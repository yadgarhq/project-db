## What

Corrects a stale claim in `src/schema.rs` and `src/path.rs`: both said
"migrations 1 and 2 named no `COLLATE`" for the two path columns. Migration
1's `path` column (`create_project`, `schema.rs`) has since been amended to
declare `COLLATE utf8mb4_general_ci` directly; migration 2's `alias_path`
still names none. No production code changed.

## Why

Ledger 1138: `schema.rs:259-261` and `path.rs:96-99` still describe both
migrations as silent on collation, but `create_project`'s SQL at
`schema.rs:160-161` already carries an explicit `COLLATE` clause on `path`.
Left uncorrected, a reader trusts the comment over the code it sits beside.

## Changelog

- docs(schema): correct the migration-4 doc comment now that migration 1 declares path's COLLATE directly
- docs(path): correct refuse_reserved_root's doc comment to match migration 1's current COLLATE declaration

## Verification

Re-read `src/schema.rs`'s `create_project` (migration 1) and
`pin_the_path_collation` (migration 4) and `src/path.rs`'s
`refuse_reserved_root` against `origin/main`, confirmed migration 1's `path`
column carries `CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci` while
migration 2's `project_alias.alias_path` still declares none. Ran `cargo fmt
--check`, `cargo clippy --all-targets -- -D warnings`, and the full `cargo
test` suite against `mariadb:11.8` — all clean, 0 failed. The diff is
comment-only; no test asserted the stale text, so none needed a red/green
cycle.

## Risk

None. Documentation-only change.
