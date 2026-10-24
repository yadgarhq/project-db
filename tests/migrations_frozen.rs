//! Stage 4 red 8 (`plans/team-access.md`): adding `project_grant` leaves every
//! earlier migration byte for byte as it shipped. A deployed store has already
//! applied migrations 1-6; an edit to one of them reaches no such store and
//! splits the schema between fresh and upgraded installs (the v0.7.0 failure
//! `tests/upgrade.rs` records).
//!
//! The digests are of each migration's SQL as `origin/main` carried it at
//! cbb262c, before stage 4. They are literals, never derived at run time.

use sha2::{Digest, Sha256};
use yadgar_project_db::schema;

const FROZEN: [(u64, &str, &str); 6] = [
    (
        1,
        "create_project",
        "1ed1e38dcedde202c5ec8bd106fef46956b6f1c2516a3e00c3e6f49ad24cb182",
    ),
    (
        2,
        "project_alias",
        "80ebb1002b92f3ef2f322aa1fd410739f29e5c58afa8b7a2289fc9b6c0dbc4cb",
    ),
    (
        3,
        "project_write_idempotency",
        "37f96261d2da1ba798aa2ac1f2fd7885ccb7b26768ed1089958fc05e62415ed7",
    ),
    (
        4,
        "pin_the_path_collation",
        "7d7c1c7693303f5bf4bf9edfea6d03d0ec53b4141f0f1bf0053d03d7b7610362",
    ),
    (
        5,
        "project_sync_lock",
        "12a18dd81053d356d53a94c194aeb4d248296b3335b05c3315635125ad9a2597",
    ),
    (
        6,
        "project_class_columns",
        "809bdb9c6917782867e512132146b0fcc917c7ad8eafe6e39d11d8bdf32728a7",
    ),
];

fn hex(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn red_08_every_earlier_migration_is_unchanged() {
    let set = schema::migrations().expect("set");
    let all: Vec<_> = set.pending_after(0).collect();
    for (version, name, digest) in FROZEN {
        let m = all
            .iter()
            .find(|m| m.version == version)
            .unwrap_or_else(|| panic!("migration {version} is gone"));
        assert_eq!(m.name, name, "migration {version} was renamed");
        assert_eq!(
            hex(&m.sql),
            digest,
            "migration {version} ({name}) was edited"
        );
    }
}

#[test]
fn migration_7_is_the_grant_table_and_is_the_last() {
    let set = schema::migrations().expect("set");
    assert_eq!(set.versions(), vec![1, 2, 3, 4, 5, 6, 7]);
    let all: Vec<_> = set.pending_after(6).collect();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "project_grant");
}
