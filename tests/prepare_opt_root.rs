//! [`PrepareOptRoot`]'s full cycle against a real filesystem, in a tempdir and
//! without root.
//!
//! the directory is really created and removed, while the user lookup and the
//! `chown` go through a mock. not a detail: since A-V3-4 the step asks the
//! system whether the user exists, and with real `SystemOps` the outcome would
//! depend on the machine running the tests.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::prepare_opt_root::PrepareOptRoot;

/// a minimal context: the step needs the home, the user and the dry-run flag.
fn ctx(home: PathBuf, dry_run: bool) -> Context {
    Context {
        odoo_home: home,
        odoo_user: "odoo".to_string(),
        dry_run,
        ..Default::default()
    }
}

/// a step with the user **absent**: the normal case, where the home stays
/// root-owned awaiting the next step.
fn step_without_user() -> PrepareOptRoot {
    let (mock, _log) = MockSystemOps::new(MockConfig::default());
    PrepareOptRoot::with_ops(Box::new(mock))
}

/// reads the `PreState` the step persisted.
fn persisted_prestate(step: &PrepareOptRoot) -> PreState {
    serde_json::from_value(step.snapshot_value()).expect("prestate serializzabile")
}

#[test]
fn created_by_us_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo"); // inesistente: parent esiste
    assert!(!home.exists());

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(home.exists(), "the run must create the directory");
    assert_eq!(persisted_prestate(&step), PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert!(
        !home.exists(),
        "the undo must remove the directory we created"
    );
}

#[test]
fn preexisting_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf(); // already exists
    assert!(home.exists());

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted_prestate(&step), PreState::Preexisting);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    // a pre-existing directory survives: not ours, not deleted.
    assert!(
        home.exists(),
        "the undo must NOT remove a Preexisting directory"
    );
}

#[test]
fn undo_does_not_force_on_non_empty_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(home.exists());

    // simulates a later step's artifact inside the directory.
    std::fs::write(home.join("intruso.txt"), b"x").expect("write file");

    // the undo is best-effort: it logs and does NOT remove.
    step.undo(&c).expect("undo best-effort");
    assert!(
        home.exists(),
        "the undo must not remove a non-empty directory (never rm -rf)"
    );
    assert!(home.join("intruso.txt").exists());
}

#[test]
fn dry_run_does_not_create() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let c = ctx(home.clone(), /* dry_run */ true);
    let mut step = step_without_user();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // a dry run creates nothing and leaves the state untracked.
    assert!(!home.exists(), "a dry run must not create the directory");
    assert_eq!(persisted_prestate(&step), PreState::Untracked);

    step.undo(&c).expect("undo");
    assert!(!home.exists());
}

// --- A-V3-4: handing over the home when the user already exists -------------

/// with the user already on the machine, the freshly created home is handed
/// over **at once**, here.
///
/// `owned root` is a waiting condition, not the home's right state. if the user
/// exists, nobody will hand it over later — the next step sees a `Preexisting`
/// user and returns — and the installation dies three steps on.
#[test]
fn an_already_existing_user_receives_the_home_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let cfg = MockConfig {
        user_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = PrepareOptRoot::with_ops(Box::new(mock));
    let c = ctx(home.clone(), false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(home.exists(), "the directory is created anyway");
    assert_eq!(
        persisted_prestate(&step),
        PreState::CreatedByUs,
        "the handover does not change ownership: the directory stays ours to remove"
    );

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::ChownNamed { path, owner, group }
                if *path == home && owner == "odoo" && group == "odoo"
        )),
        "the home must be handed to the already-existing user: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Chmod { path, mode } if *path == home && *mode == 0o750)),
        "the same permissions CreateOdooUser would set: the home must come out identical \
         whichever step handed it over: {ops:?}"
    );
}

/// without a user nothing is handed over: the normal case, where the home stays
/// root-owned until the next step creates the user and chowns it.
#[test]
fn without_the_user_the_home_stays_root_owned() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = PrepareOptRoot::with_ops(Box::new(mock));
    let c = ctx(home.clone(), false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    assert!(
        !ops.iter().any(|op| matches!(op, Op::ChownNamed { .. })),
        "no handover: the user does not exist yet, CreateOdooUser will see to it: {ops:?}"
    );
}

/// a **pre-existing** directory is handed to nobody, user or no user: it is not
/// ours. the boundary between this fix and the anti-drop rule applied to
/// directories — ownership decides, not convenience.
#[test]
fn a_preexisting_home_is_never_handed_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf(); // already exists

    let cfg = MockConfig {
        user_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = PrepareOptRoot::with_ops(Box::new(mock));
    let c = ctx(home, false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert_eq!(persisted_prestate(&step), PreState::Preexisting);
    assert!(
        ops_of(&log).is_empty(),
        "on a directory that is not ours nothing is touched: {:?}",
        ops_of(&log)
    );
}

/// a dry run neither creates nor hands over.
#[test]
fn dry_run_hands_over_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let cfg = MockConfig {
        user_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = PrepareOptRoot::with_ops(Box::new(mock));
    let c = ctx(home.clone(), true);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(!home.exists());
    assert!(ops_of(&log).is_empty(), "a dry run mutates nothing");
}
