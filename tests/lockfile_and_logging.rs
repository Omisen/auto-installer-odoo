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
        "una seconda installazione deve essere rifiutata"
    );

    drop(guard); // RAII: il lock è rilasciato al Drop.
    let _again = lockfile::acquire(&path).expect("dopo il rilascio si può riacquisire");
}

#[test]
fn log_file_open_degrades_without_failing() {
    let dir = tempfile::tempdir().expect("tempdir");
    // a writable path yields a file.
    assert!(logging::try_open(&dir.path().join("installer.log")).is_some());
    // an unwritable one yields none: it degrades rather than panics.
    assert!(
        logging::try_open(Path::new("/proc/nonexistent-dir-xyz/installer.log")).is_none(),
        "un percorso non scrivibile deve degradare a None"
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
        ("il manifesto", state::DEFAULT_STATE_PATH),
    ] {
        assert!(
            !Path::new(path).starts_with(home),
            "{what} ({path}) sta dentro {}: la directory non potrebbe più essere \
             rimossa dall'undo di prepare-opt-root (A-V3-2)",
            home.display()
        );
    }

    // the historical path does sit inside, which is why it is historical.
    assert!(
        Path::new(state::LEGACY_STATE_PATH).starts_with(home),
        "se il percorso storico non fosse più dentro {}, questa costante non \
         servirebbe più a nulla e andrebbe rimossa",
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
        "un manifesto scritto da una versione precedente deve restare consumabile"
    );

    // the previous name's manifest exists too: the more recent wins. the REAL
    // rename case.
    std::fs::write(&rinominato, b"{}").expect("write rinominato");
    assert_eq!(
        state::pick_state_path(&current, &legacy),
        rinominato,
        "fra due manifesti storici si consuma il più recente"
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
        "il percorso pre-rename deve restare fra quelli letti"
    );
    assert!(
        state::LEGACY_STATE_PATHS.contains(&state::LEGACY_STATE_PATH),
        "il percorso pre-2.2.0 deve restare fra quelli letti"
    );
    assert_eq!(
        state::LEGACY_STATE_PATHS.first(),
        Some(&state::RENAMED_STATE_PATH),
        "l'elenco va dal più recente al più vecchio: l'ordine decide quale \
         manifesto si consuma se ne esistessero due"
    );
    assert!(
        !state::DEFAULT_STATE_PATH.contains("odoo"),
        "il percorso CORRENTE non deve più portare il nome vecchio"
    );
}

/// clearing must not remove the parent of an arbitrary `--state`: the cleanup
/// is restricted to the project's own constant.
#[test]
fn clear_does_not_remove_an_arbitrary_parent_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("stato-di-qualcun-altro");
    std::fs::create_dir(&parent).expect("mkdir");
    let path = parent.join("state.json");
    std::fs::write(&path, b"{}").expect("write");

    state::InstallState::clear(&path).expect("clear");

    assert!(!path.exists(), "il file di stato deve essere rimosso");
    assert!(
        parent.exists(),
        "clear non deve rimuovere la directory genitrice di un --state arbitrario"
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
        "se non è il genitore, il ramo che rimuove la directory in clear() non \
         si attiva mai e il guscio vuoto resta sul disco"
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
        "con il genitore assente l'acquisizione deve fallire, non creare la directory"
    );
    assert!(
        !parent.exists(),
        "acquire non deve creare la directory genitrice: è così che `/opt/odoo` \
         nasceva fuori dal motore (A-V3-2)"
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
        "l'acquisizione del lock non deve aver creato la home"
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
        "con il lock fuori dal perimetro la home è nostra, non preesistente"
    );

    step.undo(&ctx).expect("undo");
    assert!(
        !home.exists(),
        "dopo il rollback la home non deve sopravvivere: è la promessa dominante del progetto"
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
        "documenta il difetto: con il lock dentro, l'undo non poteva attivarsi"
    );
}

/// the logger must not create the home: on a first installation it does not
/// exist yet, which is why A-R5-2 left the user without a file post-mortem.
/// outside it, the file appears **immediately**.
#[test]
fn log_does_not_depend_on_a_directory_the_installer_must_still_create() {
    let root = tempfile::tempdir().expect("tempdir");
    let home: PathBuf = root.path().join("odoo"); // assente, come su macchina vergine
    let var_log = root.path().join("var-log");
    std::fs::create_dir(&var_log).expect("mkdir var-log");

    // a log inside the home never appears, the old behaviour…
    assert!(
        logging::try_open(&home.join(".installer.log")).is_none(),
        "senza la home, un log al suo interno non può nascere"
    );
    // …one outside does, without bringing the home into existence.
    assert!(logging::try_open(&var_log.join("invok.log")).is_some());
    assert!(!home.exists(), "aprire il log non deve creare la home");
}
