//! [`SetupDataDir`] (A-R5-3): the filestore becomes a recorded artifact, and
//! therefore undoable — under the same protection that defends the customer's
//! database.
//!
//! the two axes to keep apart are **ownership of the directory** and
//! **ownership of the data**. both are needed: the first alone would delete a
//! pre-existing database's attachments, the second alone a directory we never
//! created.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::generate_config;
use invok::steps::setup_data_dir::{DataDirSnapshot, SetupDataDir};

/// a context over a real home, so `path_exists` really answers: the levels have
/// to be told apart one by one.
fn ctx(home: &Path, db_is_ours: bool) -> Context {
    let c = Context {
        odoo_user: "odoo".to_string(),
        odoo_home: home.to_path_buf(),
        install_dir: home.join("odoo18"),
        db_name: "odoo".to_string(),
        dry_run: false,
        ..Default::default()
    };
    c.db_created_by_us.store(db_is_ours, Ordering::SeqCst);
    c
}

fn mock() -> (MockSystemOps, common::OpLog) {
    MockSystemOps::new(MockConfig {
        real_fs: true, // path_exists guarda la tempdir vera
        ..Default::default()
    })
}

fn snapshot_of(step: &SetupDataDir) -> DataDirSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

fn removed_paths(ops: &[Op]) -> Vec<PathBuf> {
    ops.iter()
        .filter_map(|o| match o {
            Op::RemoveDirAll(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn creates_the_filestore_and_removes_the_topmost_level_it_created() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);
    let data_dir = generate_config::data_dir(&c);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Untracked);
    assert_eq!(
        snap.created_root,
        Some(home.path().join(".local")),
        "nothing existed: the root we created is `.local`"
    );
    assert!(snap.db_was_ours, "the DB was not pre-existing");

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::MkdirAsUser {
            user: "odoo".to_string(),
            path: data_dir.clone(),
        }),
        "the filestore is created as the odoo user: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        snapshot_of(&step).prestate,
        PreState::CreatedByUs,
        "after the run the directory is ours"
    );

    step.undo(&c).expect("undo");
    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".local")],
        "the undo removes the root we created, in one go"
    );
}

#[test]
fn a_preexisting_dot_local_is_not_touched() {
    // the customer already has the outer level, with other things in it. we
    // create only what is below, and the undo must stop there.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join(".local")).expect("mkdir .local");

    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        snapshot_of(&step).created_root,
        Some(home.path().join(".local").join("share")),
        "the first missing level is `share`, not `.local`"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let removed = removed_paths(&ops_of(&log));
    assert_eq!(removed, vec![home.path().join(".local").join("share")]);
    assert!(
        !removed.contains(&home.path().join(".local")),
        "a customer's pre-existing directory is never removed: {removed:?}"
    );
}

#[test]
fn a_preexisting_filestore_is_left_alone_entirely() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);
    std::fs::create_dir_all(generate_config::data_dir(&c)).expect("mkdir data_dir");

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Preexisting);
    assert_eq!(snap.created_root, None);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).is_empty(),
        "a pre-existing filestore: neither created nor removed. found: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_filestore_of_a_preexisting_database_is_never_removed() {
    // CRITICAL PROTECTION: the directory is ours, but the database was the
    // customer's and their attachments are inside. that database is not
    // dropped, and its filestore is not deleted.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), false); // DB preesistente

    step.snapshot(&c).expect("snapshot");
    assert!(!snapshot_of(&step).db_was_ours);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "the filestore of a pre-existing DB is not removed: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_database_verdict_crosses_the_disk_boundary() {
    // the real `rollback` case: the context is rebuilt from the persisted
    // config, where the flag defaults to false. reading that instead of the
    // step's own snapshot would leave our filestore behind — and inverted,
    // would remove a customer's by mistake.
    let home = tempfile::tempdir().expect("tempdir");

    // installation: our database, our filestore.
    let (ops, _log) = mock();
    let mut live = SetupDataDir::with_ops(Box::new(ops));
    let install_ctx = ctx(home.path(), true);
    live.snapshot(&install_ctx).expect("snapshot");
    live.run(&install_ctx).expect("run");
    let persisted = live.snapshot_value();

    // rollback from disk: a pristine context, the step rehydrated.
    let (ops, log) = mock();
    let mut from_disk = SetupDataDir::with_ops(Box::new(ops));
    from_disk.rehydrate(&persisted).expect("rehydrate");
    let rollback_ctx = ctx(home.path(), false);
    from_disk.undo(&rollback_ctx).expect("undo");

    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".local")],
        "the verdict on the DB is re-read from the snapshot, not derived from the Context"
    );
}

#[test]
fn a_snapshot_pointing_outside_the_perimeter_removes_nothing() {
    // fail-closed on a corrupted state, or one written by another installation:
    // a recorded root outside the home must not become a recursive removal
    // elsewhere.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));

    for hostile in ["/etc", "/", "/opt/odoo-altrove"] {
        let snapshot = serde_json::json!({
            "prestate": "CreatedByUs",
            "created_root": hostile,
            "db_was_ours": true,
        });
        step.rehydrate(&snapshot).expect("rehydrate");
        step.undo(&ctx(home.path(), true))
            .expect("undo best-effort");
    }
    // and the edge case: the recorded root is the home itself.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path(),
        "db_was_ours": true,
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&ctx(home.path(), true))
        .expect("undo best-effort");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "no removal outside the perimeter: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_mutates_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let mut c = ctx(home.path(), true);
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // a dry run never reaches `CreatedByUs`, so the state is forced to exercise
    // the removal branch too.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path().join(".local"),
        "db_was_ours": true,
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "dry run: no operation on the system. found: {:?}",
        ops_of(&log)
    );
}
