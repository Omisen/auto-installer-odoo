//! [`PrepareOptRoot`]'s full cycle against a real filesystem, in a tempdir and
//! without root.
//!
//! the directory is really created and removed, while the user lookup and the
//! `chown` go through a mock. not a detail: since A-V3-4 the step asks the
//! system whether the user exists, and with real `SystemOps` the outcome would
//! depend on the machine running the tests.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::prepare_opt_root::{held_mode_notice, OptRootSnapshot, PrepareOptRoot};

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

/// reads the shared root's `PreState` from what the step persisted.
///
/// since I0 the snapshot carries two levels — the shared `/opt/odoo` and, for a
/// named instance, that instance's own home. these tests all describe the
/// unnamed instance, where only the first one is in play.
fn persisted_prestate(step: &PrepareOptRoot) -> PreState {
    let snap: OptRootSnapshot =
        serde_json::from_value(step.snapshot_value()).expect("prestate serializzabile");
    snap.shared_root
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

// --- I0: the second level, for a named instance -----------------------------

/// a context for a **named** instance: the shared root and this instance's own
/// home are two different directories, and the step owns both.
fn ctx_named(shared_root: PathBuf, instance: &str) -> Context {
    let install_dir = shared_root.join(format!("odoo-{instance}"));
    Context {
        odoo_home: shared_root,
        install_dir,
        instance: Some(instance.to_string()),
        odoo_user: format!("odoo-{instance}"),
        dry_run: false,
        ..Default::default()
    }
}

/// reads both levels the step persisted.
fn persisted(step: &PrepareOptRoot) -> OptRootSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

#[test]
fn a_named_instance_creates_both_levels_and_removes_both() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    let c = ctx_named(shared.clone(), "cliente-x");
    let home = c.user_home();
    assert!(!shared.exists());

    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(shared.exists(), "the shared root must be created");
    assert!(home.exists(), "the instance's own home must be created too");
    let snap = persisted(&step);
    assert_eq!(snap.shared_root, PreState::CreatedByUs);
    assert_eq!(snap.instance_home, PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert!(!home.exists(), "the instance home must come off");
    assert!(
        !shared.exists(),
        "and with the last instance gone, so must the shared root"
    );
}

/// the case that makes the two levels necessary: `/opt/odoo` was already there,
/// because another instance created it. this instance's rollback must take its
/// own home and **leave the shared root alone**.
///
/// with a single `PreState` for both, the second instance's rollback either
/// spared its own home or destroyed the ground the first one stands on.
#[test]
fn a_preexisting_shared_root_survives_the_instance_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("the other instance created it");

    let c = ctx_named(shared.clone(), "cliente-x");
    let home = c.user_home();

    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let snap = persisted(&step);
    assert_eq!(
        snap.shared_root,
        PreState::Preexisting,
        "we did not create /opt/odoo, so it is not ours to remove"
    );
    assert_eq!(snap.instance_home, PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert!(!home.exists(), "our own home comes off");
    assert!(
        shared.exists(),
        "the shared root must survive: another instance lives under it"
    );
}

/// the unnamed instance has **one** level, and recording a second would give
/// the undo two claims on one directory.
#[test]
fn the_unnamed_instance_still_has_a_single_level() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let snap = persisted(&step);
    assert_eq!(snap.shared_root, PreState::CreatedByUs);
    assert_eq!(
        snap.instance_home,
        PreState::Untracked,
        "its home IS the shared root: there is no second directory to own"
    );
}

/// a manifest written before I0 stored a bare `PreState` for this step. it must
/// still rehydrate, and the undo must still fire.
///
/// this is not politeness towards old files: a snapshot that cannot be read is
/// an undo that is **skipped** (fail-closed), so a rename of the snapshot shape
/// would leave every instance installed before I0 with `/opt/odoo` on disk
/// forever — A-V3-1's harm, arriving through a refactor.
#[test]
fn a_pre_i0_snapshot_is_still_readable_and_its_undo_still_fires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");
    std::fs::create_dir(&home).expect("mkdir");

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();
    // exactly what versions before I0 wrote: the enum, on its own.
    step.rehydrate(&serde_json::json!("CreatedByUs"))
        .expect("a pre-I0 snapshot must stay readable");

    assert_eq!(persisted(&step).shared_root, PreState::CreatedByUs);
    step.undo(&c).expect("undo");
    assert!(
        !home.exists(),
        "the undo of a rehydrated pre-I0 snapshot must still remove the directory"
    );
}

/// and the current shape round-trips through the same door.
#[test]
fn the_two_level_snapshot_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    let c = ctx_named(shared, "cliente-x");

    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    let written = step.snapshot_value();

    let mut rehydrated = step_without_user();
    rehydrated.rehydrate(&written).expect("rehydrate");
    assert_eq!(persisted(&rehydrated), persisted(&step));
}

// --- I2: the shared root belongs to every instance --------------------------

/// this step owns **both** the shared root and this instance's own home, which
/// is why the rollback driver still calls it when another instance is installed
/// instead of skipping it whole. it must do its own half and leave the other.
///
/// skipping it entirely would leave the instance's home behind; running it
/// entirely would remove the directory every other instance lives under.
#[test]
fn with_another_instance_installed_only_the_instance_home_comes_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    let mut c = ctx_named(shared.clone(), "cliente-x");
    let home = c.user_home();

    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(shared.exists() && home.exists());

    // somebody else is still installed.
    c.other_instances = vec!["beta".to_string()];
    step.undo(&c).expect("undo");

    assert!(
        !home.exists(),
        "the instance's own home is nobody else's business: it goes"
    );
    assert!(
        shared.exists(),
        "the shared root stays while another instance lives under it, whoever created it"
    );

    // and once it is alone, the same undo finishes the job: the undos are
    // idempotent, so the second run is not a special case.
    c.other_instances.clear();
    step.undo(&c).expect("undo");
    assert!(
        !shared.exists(),
        "with nobody left, the instance that created the shared root removes it"
    );
}

/// the unnamed instance's home **is** the shared root, so there is no own half
/// to salvage — which is why `artifact_scope` calls it `Shared` there and the
/// driver does not call this undo at all.
#[test]
fn for_the_unnamed_instance_the_shared_root_is_the_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("odoo");
    let mut c = ctx(home.clone(), false);

    let mut step = step_without_user();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    c.other_instances = vec!["beta".to_string()];
    step.undo(&c).expect("undo");
    assert!(
        home.exists(),
        "were the driver ever to call it anyway, the flag alone must still protect the \
         directory the other instances live under"
    );
}

// --- A-V6-9: the shared root has to be walkable by everybody under it -------

/// a mock that answers about the **real** filesystem, so mode and `chmod` agree
/// with what the step actually did.
fn step_on_real_fs() -> (PrepareOptRoot, common::OpLog) {
    let (mock, log) = MockSystemOps::new(MockConfig {
        real_fs: true,
        ..Default::default()
    });
    (PrepareOptRoot::with_ops(Box::new(mock)), log)
}

fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o7777
}

/// the finding itself: `/opt/odoo` handed to the unnamed instance is `0750`, and
/// a named instance's user is neither its owner nor in its group — so without
/// this it cannot reach its own home, and fails three steps later with a
/// `mkdir` error that names a directory which exists.
#[test]
fn a_named_instance_widens_a_shared_root_it_could_not_traverse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let c = ctx_named(shared.clone(), "cliente-x");
    let (mut step, _log) = step_on_real_fs();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        mode_of(&shared),
        0o750,
        "the snapshot only reads: the mutation is the run's (C4)"
    );

    step.run(&c).expect("run");
    assert_eq!(
        mode_of(&shared),
        0o751,
        "one bit, and only that one: traverse yes, list no"
    );
    assert_eq!(
        persisted(&step).shared_root_mode_before,
        Some(0o750),
        "the mode found is what the undo has to put back, so it must be persisted"
    );

    // and the promise: alone on the machine, the customer's directory goes back
    // to the permissions it had.
    step.undo(&c).expect("undo");
    assert_eq!(mode_of(&shared), 0o750, "the widening is ours to take back");
    assert!(shared.exists(), "the directory itself was never ours");
}

/// the reason this could not be done before I2: putting the mode back while
/// another instance is still installed locks *that* instance out of its own
/// home — the very failure, caused by the fix for it.
#[test]
fn the_widening_stays_while_another_instance_still_needs_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let mut c = ctx_named(shared.clone(), "cliente-x");
    let (mut step, _log) = step_on_real_fs();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    c.other_instances = vec!["beta".to_string()];
    step.undo(&c).expect("undo");
    assert_eq!(
        mode_of(&shared),
        0o751,
        "somebody else is still living under it and still has to walk in"
    );
}

/// a root that is already traversable is not touched, and nothing is recorded:
/// there is no undo to owe when there was no mutation.
#[test]
fn a_traversable_root_is_left_exactly_as_it_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let c = ctx_named(shared.clone(), "cliente-x");
    let (mut step, log) = step_on_real_fs();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert_eq!(persisted(&step).shared_root_mode_before, None);
    assert_eq!(mode_of(&shared), 0o755);
    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::Chmod { path, .. } if path == &shared)),
        "no chmod at all on a root that was already fine"
    );
}

/// the unnamed instance **owns** the shared root — it is its home — so widening
/// it would open one instance's private directory to third parties for nobody's
/// benefit.
#[test]
fn the_unnamed_instance_never_widens_its_own_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("opt-odoo");
    std::fs::create_dir(&home).expect("mkdir");
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let c = ctx(home.clone(), false);
    let (mut step, _log) = step_on_real_fs();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert_eq!(persisted(&step).shared_root_mode_before, None);
    assert_eq!(mode_of(&home), 0o750, "its own home, its own permissions");
}

/// an unreadable mode is "I do not know", never "it is fine": widening without
/// having read what was there is a mutation with no undo, so the installation
/// stops **before** anything is touched — and the message names the permission,
/// which is what the field failure did not.
#[test]
fn an_unreadable_mode_stops_the_installation_before_it_mutates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");

    let (mock, _log) = MockSystemOps::new(MockConfig {
        real_fs: true,
        mode_unreadable: true,
        ..Default::default()
    });
    let mut step = PrepareOptRoot::with_ops(Box::new(mock));
    let c = ctx_named(shared.clone(), "cliente-x");

    let message = step
        .snapshot(&c)
        .expect_err("an unreadable mode must stop the installation")
        .to_string();
    assert!(
        message.contains("permissions") && message.contains("odoo-cliente-x"),
        "the message must name the permission and whose traversal it blocks:\n{message}"
    );
}

/// the `.bashrc` rule, applied to a mode: we put back what we changed, or we
/// leave it alone — never a value somebody else may have chosen since.
#[test]
fn a_mode_changed_by_somebody_else_is_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let c = ctx_named(shared.clone(), "cliente-x");
    let (mut step, _log) = step_on_real_fs();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // the administrator has since decided otherwise.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o775)).expect("chmod");
    step.undo(&c).expect("undo");
    assert_eq!(
        mode_of(&shared),
        0o775,
        "their decision stands: our undo only takes back the mode it left"
    );
}

/// the distinction the *names* exist for, and the common case: remove the added
/// instance from a customer's machine and the historical one is all that is
/// left — it **owns** `/opt/odoo`, so it never needed the `o+x`, and the
/// customer gets the `0750` they had back.
///
/// a boolean "is anybody else installed" answers yes here and keeps a permission
/// bit nobody uses: the same datum asked a question it was not written for.
#[test]
fn only_the_unnamed_instance_left_means_the_widening_comes_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared = dir.path().join("opt-odoo");
    std::fs::create_dir(&shared).expect("mkdir");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).expect("chmod");

    let mut c = ctx_named(shared.clone(), "cliente-x");
    let (mut step, _log) = step_on_real_fs();
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert_eq!(mode_of(&shared), 0o751);

    c.other_instances = vec!["default".to_string()];
    step.undo(&c).expect("undo");

    assert_eq!(
        mode_of(&shared),
        0o750,
        "the historical instance is the root's owner: it walks in as itself"
    );
    assert!(
        shared.exists(),
        "and the directory stays: somebody is still installed on it"
    );
}

// --- the bit that outlives its instance is accepted, but not silent ----------

/// **A-V6-11-bis**: when the traversal bit is kept on purpose, the undo says so.
///
/// the residue itself is a decision, not a defect: closing it would mean a
/// refcount on a permission, for one *traversal* bit on a directory whose
/// contents stay `0750`. what was wrong is that the skipped restoration said
/// **nothing at all** — the customer removed an instance, saw `/opt/odoo` at
/// `0751`, and had no way to tell our artifact from their own configuration.
///
/// asserted on the returned text and not on a captured log, for the reason
/// A-R9-1 taught: when the value of a message is its wording, checking that
/// something happened checks nothing.
#[test]
fn a_traversal_bit_kept_on_purpose_says_so_and_names_who_needs_it() {
    let notice = held_mode_notice(Some(0o750), &["beta".to_string(), "gamma".to_string()])
        .expect("a widened root with named neighbours must be accounted for");

    assert!(
        notice.contains("751") && notice.contains("750"),
        "both modes must appear, or the reader cannot tell what is held from what is owed: \
         {notice}"
    );
    assert!(
        notice.contains("beta") && notice.contains("gamma"),
        "naming who still needs it is the difference between an explanation and an excuse: \
         {notice}"
    );
    assert!(
        notice.contains("restored when the last"),
        "the reader must learn the bit is not permanent: {notice}"
    );
}

/// and it stays quiet in the two cases where that sentence would be false.
///
/// nothing widened by us — there is no bit of ours to hold; and neighbours that
/// are only the **unnamed** instance, which owns the root and never needed the
/// bit, so it is not what keeps it (`A-V6-9`'s distinction, the one that
/// A-R8-1 was repeated over).
#[test]
fn nothing_is_announced_when_there_is_nothing_being_held() {
    assert!(
        held_mode_notice(None, &["beta".to_string()]).is_none(),
        "we widened nothing: there is no bit of ours to account for"
    );
    assert!(
        held_mode_notice(Some(0o750), &[]).is_none(),
        "nobody else is here at all"
    );
    assert!(
        held_mode_notice(Some(0o750), &[invok::instance::UNNAMED_ID.to_string()]).is_none(),
        "the unnamed instance owns the root and walks in as itself: claiming it keeps the bit \
         would be the very confusion A-V6-9 was corrected for"
    );
}

/// and the notice is **reached** from the undo.
///
/// the pure tests above prove the sentence is right; they cannot prove anybody
/// says it. mutation showed exactly that: deleting the call from `undo` left
/// every one of them green — which is the objection this project raises against
/// its own diagnostics, *a correct message nobody invokes is indistinguishable
/// from an absent one* (A-MD-7, A-R9-1), turned on the test that was supposed to
/// guard it.
///
/// structural because there is nothing else to read: the notice goes to
/// `tracing`, and this suite captures no logs. It is the pair R9 prescribes —
/// the grep sees the shape of the code, the tests above see the value it
/// produces — and neither alone would have caught the mutation.
#[test]
fn the_undo_actually_announces_the_bit_it_keeps() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/steps/prepare_opt_root.rs"),
    )
    .expect("src/steps/prepare_opt_root.rs must be readable");

    let undo = source
        .split_once("fn undo(&self, ctx: &Context)")
        .expect("the step must still have an undo")
        .1;
    // up to the next method: enough to be sure the call is on this path and not
    // somewhere else in the file.
    let body = undo.split_once("\n    fn ").map(|(b, _)| b).unwrap_or(undo);

    assert!(
        body.contains("announce_mode_held"),
        "the undo no longer accounts for a traversal bit it keeps: the customer is left with a \
         0751 they cannot attribute. undo body was:\n{body}"
    );
}
