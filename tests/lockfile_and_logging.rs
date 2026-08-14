//! the lock file (G5) and the file logger's graceful degradation (G1), plus the
//! guards on **where** they are allowed to live (A-V3-2).

use std::path::{Path, PathBuf};

use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::prepare_opt_root::PrepareOptRoot;
use invok::{config, lockfile, logging, state};

#[test]
fn second_concurrent_lock_is_refused_and_released_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("installer.lock");

    let guard = lockfile::acquire(&path).expect("primo lock acquisito");
    // a second run, a new descriptor on the same file, is refused.
    assert!(
        lockfile::acquire(&path).is_err(),
        "a second installation must be refused"
    );

    drop(guard); // RAII: the lock is released on Drop.
    let _again = lockfile::acquire(&path).expect("after the release it can be acquired again");
}

#[test]
fn log_file_open_degrades_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    // a writable path yields a file.
    assert!(logging::try_open(&dir.path().join("installer.log")).is_some());
    // an unwritable one yields none: it degrades rather than panics.
    assert!(
        logging::try_open(Path::new("/proc/nonexistent-dir-xyz/installer.log")).is_none(),
        "a non-writable path must degrade to None"
    );
}

// --- A-V3-2: the lock and the log must not create the home ------------------
//
// the defect lived entirely in `main`, between pieces each covered on its own:
// acquiring the lock created the home before the engine started, so the first
// step saw it as pre-existing, its undo became a no-op, and the directory
// survived every rollback. the three guards below cover the three shapes the
// defect can return in: the path, the implicit creation, and the real order of
// operations.

/// the **path** guard: no bookkeeping artifact may live inside the perimeter
/// the engine has to remove.
///
/// the most direct form of the defect: move any of these constants back under
/// the home and the first step's undo becomes unreachable — either because the
/// directory is born before the engine, or because the last undo finds it
/// occupied.
#[test]
fn installer_bookkeeping_lives_outside_the_reversible_perimeter() {
    let home = Path::new(config::ODOO_HOME);
    for (what, path) in [
        ("il lock", lockfile::DEFAULT_LOCK_PATH),
        ("il log", logging::DEFAULT_LOG_PATH),
        ("the manifest", state::DEFAULT_STATE_PATH),
    ] {
        assert!(
            !Path::new(path).starts_with(home),
            "{what} ({path}) lives inside {}: the directory could no longer be removed by \
             prepare-opt-root's undo (A-V3-2)",
            home.display()
        );
    }

    // the historical path does sit inside, which is why it is historical.
    assert!(
        Path::new(state::LEGACY_STATE_PATH).starts_with(home),
        "if the historical path were no longer inside {}, this constant would serve no \
         purpose and should be removed",
        home.display()
    );
}

/// the historical manifest stays **readable**: an instance installed by an
/// earlier version must stay uninstallable. checked against fixture paths, not
/// the machine's filesystem.
#[test]
fn the_legacy_manifest_is_still_found_when_the_current_one_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let current = dir.path().join("var-lib-invok").join("state.json");
    // the two historical places, newest first: the rename added the first.
    let rinominato = dir.path().join("var-lib-odoo-installer").join("state.json");
    let storico = dir.path().join("opt-odoo").join(".installer-state.json");
    for p in [&current, &rinominato, &storico] {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    }
    let legacy: Vec<&Path> = vec![&rinominato, &storico];

    // none exists: name the current one, so the "nothing to undo" message
    // points where the user should look.
    assert_eq!(state::pick_state_path(&current, &legacy), current);

    // only the oldest exists: that one is consumed.
    std::fs::write(&storico, b"{}").expect("write storico");
    assert_eq!(
        state::pick_state_path(&current, &legacy),
        storico,
        "a manifest written by an earlier version must stay consumable"
    );

    // the previous name's manifest exists too: the more recent wins. the REAL
    // rename case.
    std::fs::write(&rinominato, b"{}").expect("write rinominato");
    assert_eq!(
        state::pick_state_path(&current, &legacy),
        rinominato,
        "between two historical manifests the most recent is consumed"
    );

    // the current one exists: it always wins.
    std::fs::write(&current, b"{}").expect("write current");
    assert_eq!(state::pick_state_path(&current, &legacy), current);
}

/// the pre-rename manifest is still in the historical list.
///
/// an explicit guard, because losing it has no visible consequence here:
/// customer machines are not renamed along with the repository, and a manifest
/// that stops being read is an instance nobody can uninstall without guessing
/// artifact names.
#[test]
fn the_pre_rename_manifest_path_is_still_read() {
    assert!(
        state::LEGACY_STATE_PATHS.contains(&state::RENAMED_STATE_PATH),
        "the pre-rename path must stay among the ones read"
    );
    assert!(
        state::LEGACY_STATE_PATHS.contains(&state::LEGACY_STATE_PATH),
        "the pre-2.2.0 path must stay among the ones read"
    );
    assert_eq!(
        state::LEGACY_STATE_PATHS.first(),
        Some(&state::RENAMED_STATE_PATH),
        "the list goes newest to oldest: the order decides which manifest is consumed if \
         two existed"
    );
    assert!(
        !state::DEFAULT_STATE_PATH.contains("odoo"),
        "the CURRENT path must no longer carry the old name"
    );
}

/// clearing must not remove the parent of an arbitrary `--state`: the cleanup
/// is restricted to the project's own constant.
#[test]
fn clear_does_not_remove_an_arbitrary_parent_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("somebody-elses-state");
    std::fs::create_dir(&parent).expect("mkdir");
    let path = parent.join("state.json");
    std::fs::write(&path, b"{}").expect("write");

    state::InstallState::clear(&path).expect("clear");

    assert!(!path.exists(), "the state file must be removed");
    assert!(
        parent.exists(),
        "clear must not remove the parent directory of an arbitrary --state"
    );
}

/// the two constants must agree, or the directory cleanup is code that cannot
/// execute — this project's recurring signature.
///
/// the positive case is not exercised against the real path: these tests never
/// touch the system.
#[test]
fn the_state_dir_constant_is_actually_the_parent_of_the_state_file() {
    assert_eq!(
        Path::new(state::DEFAULT_STATE_PATH).parent(),
        Some(Path::new(state::DEFAULT_STATE_DIR)),
        "if it is not the parent, the branch in clear() that removes the directory never \
         fires and the empty shell stays on disk"
    );
}

/// the **implicit creation** guard: taking a lock must not bring directories
/// into existence. even with the right path, a courtesy `create_dir_all` would
/// rebuild the defect the day the path changes.
#[test]
fn acquire_does_not_create_the_parent_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("non-esiste");
    let path = parent.join("installer.lock");

    let result = lockfile::acquire(&path);

    assert!(
        result.is_err(),
        "with the parent missing the acquisition must fail, not create the directory"
    );
    assert!(
        !parent.exists(),
        "acquire must not create the parent directory: that is how the perimeter came \
         into existence outside the engine (A-V3-2)"
    );
}

/// the **real order** guard: replays the install sequence — lock first, then
/// the engine — and checks the first step reaches `CreatedByUs` and its undo
/// really removes the directory.
///
/// the missing test: the step's own file exercises it on a pristine tempdir,
/// but nobody put the lock in front of the engine, which is exactly what
/// happens in production.
#[test]
fn opt_root_is_created_by_us_even_with_the_lock_acquired_first() {
    let root = tempfile::tempdir().expect("tempdir");

    // a fake runtime dir: already there, outside the home, untouched.
    let run_dir = root.path().join("run");
    std::fs::create_dir(&run_dir).expect("mkdir run");
    let lock_path = run_dir.join("invok.lock");

    // a fake home that does NOT exist, as on a pristine machine.
    let home = root.path().join("opt").join("odoo");
    std::fs::create_dir(home.parent().expect("parent")).expect("mkdir opt");
    assert!(!home.exists());

    // the install order: the lock first…
    let _guard = lockfile::acquire(&lock_path).expect("lock acquisito");
    assert!(
        !home.exists(),
        "acquiring the lock must not have created the home"
    );

    // …then the engine.
    let ctx = Context {
        odoo_home: home.clone(),
        dry_run: false,
        ..Default::default()
    };
    let mut step = PrepareOptRoot::with_ops(Box::new(invok::system_ops::RealSystemOps::debian()));
    step.snapshot(&ctx).expect("snapshot");
    step.run(&ctx).expect("run");

    let prestate: PreState =
        serde_json::from_value(step.snapshot_value()).expect("prestate serializzabile");
    assert_eq!(
        prestate,
        PreState::CreatedByUs,
        "with the lock outside the perimeter the home is ours, not pre-existing"
    );

    step.undo(&ctx).expect("undo");
    assert!(
        !home.exists(),
        "after the rollback the home must not survive: that is the project's dominant promise"
    );
}

/// the original defect, reproduced **by contrast**: with the lock inside the
/// home, the undo becomes a no-op and the directory survives.
///
/// this does not describe wanted behaviour — it describes the bug, to show the
/// guards above measure something real.
#[test]
fn lock_inside_the_home_is_what_made_the_undo_dead_code() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("odoo");
    let lock_path = home.join(".installer.lock");

    // the old acquire did exactly this, implicitly.
    std::fs::create_dir_all(home.parent().expect("parent")).expect("mkdir");
    std::fs::create_dir(&home).expect("mkdir home");
    let _guard = lockfile::acquire(&lock_path).expect("lock acquisito");

    let ctx = Context {
        odoo_home: home.clone(),
        dry_run: false,
        ..Default::default()
    };
    let mut step = PrepareOptRoot::with_ops(Box::new(invok::system_ops::RealSystemOps::debian()));
    step.snapshot(&ctx).expect("snapshot");
    step.run(&ctx).expect("run");
    step.undo(&ctx).expect("undo");

    let prestate: PreState =
        serde_json::from_value(step.snapshot_value()).expect("prestate serializzabile");
    assert_eq!(prestate, PreState::Preexisting);
    assert!(
        home.exists(),
        "it documents the defect: with the lock inside, the undo could never fire"
    );
}

/// the logger must not create the home: on a first installation it does not
/// exist yet, which is why A-R5-2 left the user without a file post-mortem.
/// outside it, the file appears **immediately**.
#[test]
fn log_does_not_depend_on_a_directory_the_installer_must_still_create() {
    let root = tempfile::tempdir().expect("tempdir");
    let home: PathBuf = root.path().join("odoo"); // absent, as on a virgin machine
    let var_log = root.path().join("var-log");
    std::fs::create_dir(&var_log).expect("mkdir var-log");

    // a log inside the home never appears, the old behaviour…
    assert!(
        logging::try_open(&home.join(".installer.log")).is_none(),
        "without the home, a log inside it cannot come into existence"
    );
    // …one outside does, without bringing the home into existence.
    assert!(logging::try_open(&var_log.join("invok.log")).is_some());
    assert!(!home.exists(), "opening the log must not create the home");
}
