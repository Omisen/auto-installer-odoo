//! [`SetupCacheDir`] (A-R5-3, second half): the cache inside the `odoo` user's
//! home becomes an owned artifact, and therefore undoable.
//!
//! the question this step changes is not "who wrote into the cache" — they are
//! third-party programs whose behaviour varies — but "**whose** is the
//! directory". the number of producers becomes irrelevant.

mod common;

use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::setup_cache_dir::{CacheDirSnapshot, SetupCacheDir};

fn ctx(home: &Path) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_home: home.to_path_buf(),
        install_dir: home.join("odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn mock() -> (MockSystemOps, common::OpLog) {
    MockSystemOps::new(MockConfig {
        real_fs: true, // `path_exists` must look at the real tempdir
        ..Default::default()
    })
}

fn snapshot_of(step: &SetupCacheDir) -> CacheDirSnapshot {
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
fn a_cache_we_created_is_removed_by_the_rollback() {
    // the field case: after a complete installation the cache stayed because no
    // step claimed it. now it is ours, and it goes.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Untracked);
    assert_eq!(snap.created_root, Some(home.path().join(".cache")));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::MkdirAsUser {
            user: "odoo".to_string(),
            path: home.path().join(".cache"),
        }),
        "la cache va creata come utente odoo: chi ci scriverà è lui, non root. {:?}",
        ops_of(&log)
    );
    assert_eq!(snapshot_of(&step).prestate, PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".cache")]
    );
}

#[test]
fn a_preexisting_cache_belongs_to_the_client_and_is_never_touched() {
    // the home may be pre-existing with the customer's things inside. an
    // already-present cache is neither created nor removed.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join(".cache")).expect("mkdir .cache");

    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Preexisting);
    assert_eq!(snap.created_root, None);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).is_empty(),
        "cache preesistente: né creata né rimossa. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_verdict_crosses_the_disk_boundary() {
    // a rollback from disk rebuilds the step from scratch: without rehydrating
    // the `PreState` the undo would be inert and the cache would stay — the
    // very leftover this step exists for.
    let home = tempfile::tempdir().expect("tempdir");

    let (ops, _log) = mock();
    let mut live = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());
    live.snapshot(&c).expect("snapshot");
    live.run(&c).expect("run");
    let persisted = live.snapshot_value();

    let (ops, log) = mock();
    let mut from_disk = SetupCacheDir::with_ops(Box::new(ops));
    from_disk.rehydrate(&persisted).expect("rehydrate");
    from_disk.undo(&c).expect("undo");

    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".cache")],
        "lo stato persistito deve bastare a sapere che la cache è nostra"
    );
}

#[test]
fn a_snapshot_pointing_outside_the_home_removes_nothing() {
    // the same net as the filestore step: a recursive removal driven by a path
    // from disk, where a corrupted state must not become a disaster.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));

    for hostile in ["/etc", "/", "/opt/odoo-altrove"] {
        let snapshot = serde_json::json!({
            "prestate": "CreatedByUs",
            "created_root": hostile,
        });
        step.rehydrate(&snapshot).expect("rehydrate");
        step.undo(&ctx(home.path())).expect("undo best-effort");
    }
    // and the edge case: the recorded root is the home itself.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path(),
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&ctx(home.path())).expect("undo best-effort");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "nessuna rimozione fuori dal perimetro della home: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_mutates_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let mut c = ctx(home.path());
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // a dry run never reaches `CreatedByUs`, so the state is forced to exercise
    // the removal branch too.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path().join(".cache"),
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "dry-run: nessuna operazione. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_cache_is_undone_after_everything_that_could_write_into_it() {
    // the order is not a detail: undos run backwards, so this step must sit
    // EARLY to be removed LATE — after the service is stopped and the venv is
    // gone, when nothing can recreate the cache we just deleted.
    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("la famiglia Debian ha un backend");
    let names = invok::steps::canonical_step_names(&make_ops);
    let pos = |name: &str| {
        names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("'{name}' deve essere nella sequenza canonica"))
    };

    let cache = pos("setup-cache-dir");
    for later in [
        "install-python-requirements", // pip scrive cache
        "initialize-odoo-database",    // odoo-bin carica font, ecc.
        "setup-systemd",               // il servizio gira e scrive
        "create-virtualenv",
    ] {
        assert!(
            cache < pos(later),
            "setup-cache-dir deve precedere '{later}' nella sequenza, così il suo undo \
             gira dopo quello di '{later}'"
        );
    }
}
