//! Test di lockfile (G5) e degrado del logging su file (G1), più le guardie
//! su **dove** lock e log hanno diritto di vivere (A-V3-2).

use std::path::{Path, PathBuf};

use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::prepare_opt_root::PrepareOptRoot;
use odoo_installer::{config, lockfile, logging, state};

#[test]
fn second_concurrent_lock_is_refused_and_released_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("installer.lock");

    let guard = lockfile::acquire(&path).expect("primo lock acquisito");
    // Una seconda esecuzione (nuovo descriptor sullo stesso file) è rifiutata.
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
    // Percorso scrivibile → Some.
    assert!(logging::try_open(&dir.path().join("installer.log")).is_some());
    // Percorso non scrivibile → None (degrada, non panica).
    assert!(
        logging::try_open(Path::new("/proc/nonexistent-dir-xyz/installer.log")).is_none(),
        "un percorso non scrivibile deve degradare a None"
    );
}

// --- A-V3-2: lock e log non devono far nascere `/opt/odoo` -------------------
//
// Il difetto stava tutto in `main`, fra pezzi che i test coprivano
// singolarmente: `lockfile::acquire` creava `/opt/odoo` prima che il motore
// partisse, e da lì `PrepareOptRoot` vedeva la directory come `Preexisting` —
// undo NO-OP, `/opt/odoo` sopravvive a ogni rollback. Le tre guardie qui sotto
// coprono le tre forme in cui il difetto può tornare: il percorso, la creazione
// implicita, e l'ordine reale delle operazioni.

/// Guardia sul **percorso**: nessun artefatto di contabilità dell'installer può
/// stare dentro il perimetro che il motore deve saper rimuovere.
///
/// È la forma più diretta del difetto: basta riportare una di queste costanti
/// sotto `/opt/odoo` e l'undo di `PrepareOptRoot` torna irraggiungibile — o
/// perché la directory nasce prima del motore (lock, log), o perché all'ultimo
/// undo la trova occupata (manifesto). Vale contro `config::ODOO_HOME`, che è
/// dichiarata non sovrascrivibile.
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

    // Il percorso storico invece ci sta dentro: è la ragione per cui è storico.
    assert!(
        Path::new(state::LEGACY_STATE_PATH).starts_with(home),
        "se il percorso storico non fosse più dentro {}, questa costante non \
         servirebbe più a nulla e andrebbe rimossa",
        home.display()
    );
}

/// Il manifesto storico resta **leggibile**: un'istanza installata da una
/// versione precedente deve restare disinstallabile. Regola verificata con
/// percorsi di prova, non contro il filesystem della macchina.
#[test]
fn the_legacy_manifest_is_still_found_when_the_current_one_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let current = dir.path().join("var-lib").join("state.json");
    let legacy = dir.path().join("opt-odoo").join(".installer-state.json");
    std::fs::create_dir_all(current.parent().expect("parent")).expect("mkdir");
    std::fs::create_dir_all(legacy.parent().expect("parent")).expect("mkdir");

    // Nessuno dei due esiste → si nomina quello corrente, così il messaggio
    // "nessuna installazione da annullare" indica dove l'utente deve guardare.
    assert_eq!(state::pick_state_path(&current, &legacy), current);

    // Solo lo storico esiste → si consuma quello (istanza pre-migrazione).
    std::fs::write(&legacy, b"{}").expect("write legacy");
    assert_eq!(
        state::pick_state_path(&current, &legacy),
        legacy,
        "un manifesto scritto da una versione precedente deve restare consumabile"
    );

    // Esistono entrambi → vince il corrente.
    std::fs::write(&current, b"{}").expect("write current");
    assert_eq!(state::pick_state_path(&current, &legacy), current);
}

/// `clear` non deve rimuovere la directory genitrice di un `--state`
/// qualunque: la pulizia è ristretta alla costante del progetto.
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

/// Le due costanti devono essere coerenti, o la pulizia della directory è
/// codice che non può eseguire — la firma ricorrente dei difetti di questo
/// progetto (un controllo che non poteva fallire).
///
/// Non si verifica il caso positivo eseguendo `clear` sul percorso reale: gli
/// unici test che questo progetto esegue non toccano il sistema.
#[test]
fn the_state_dir_constant_is_actually_the_parent_of_the_state_file() {
    assert_eq!(
        Path::new(state::DEFAULT_STATE_PATH).parent(),
        Some(Path::new(state::DEFAULT_STATE_DIR)),
        "se non è il genitore, il ramo che rimuove la directory in clear() non \
         si attiva mai e il guscio vuoto resta sul disco"
    );
}

/// Guardia sulla **creazione implicita**: prendere un lock non deve far nascere
/// directory. Anche con un percorso corretto, un `create_dir_all` di cortesia
/// rimetterebbe in piedi il difetto il giorno in cui il percorso cambia.
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

/// Guardia sull'**ordine reale**: replica la sequenza di `run_install` — prima
/// il lock, poi il motore — e verifica che `PrepareOptRoot` arrivi a
/// `CreatedByUs` e che il suo undo rimuova davvero la directory.
///
/// È il test che mancava: `tests/prepare_opt_root.rs` esercita lo step su una
/// tempdir vergine, ma nessuno metteva il lockfile davanti al motore, che è
/// esattamente ciò che succede in produzione.
#[test]
fn opt_root_is_created_by_us_even_with_the_lock_acquired_first() {
    let root = tempfile::tempdir().expect("tempdir");

    // `/run` finto: esiste già, è fuori dalla home, e non lo tocca nessuno.
    let run_dir = root.path().join("run");
    std::fs::create_dir(&run_dir).expect("mkdir run");
    let lock_path = run_dir.join("odoo-installer.lock");

    // `/opt/odoo` finto: NON esiste, come su una macchina vergine.
    let home = root.path().join("opt").join("odoo");
    std::fs::create_dir(home.parent().expect("parent")).expect("mkdir opt");
    assert!(!home.exists());

    // Ordine di `run_install`: prima il lock…
    let _guard = lockfile::acquire(&lock_path).expect("lock acquisito");
    assert!(
        !home.exists(),
        "l'acquisizione del lock non deve aver creato la home"
    );

    // …poi il motore.
    let ctx = Context {
        odoo_home: home.clone(),
        dry_run: false,
        ..Default::default()
    };
    let mut step =
        PrepareOptRoot::with_ops(Box::new(odoo_installer::system_ops::RealSystemOps::debian()));
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

/// Il difetto originale, riprodotto per **contrasto**: se il lock vive dentro la
/// home, l'undo diventa NO-OP e la directory sopravvive.
///
/// Questo test non descrive un comportamento voluto — descrive il bug. Serve a
/// dimostrare che le guardie sopra misurano qualcosa di reale: senza di esse
/// questo è ciò che succedeva in produzione a ogni singola esecuzione.
#[test]
fn lock_inside_the_home_is_what_made_the_undo_dead_code() {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("odoo");
    let lock_path = home.join(".installer.lock");

    // Il vecchio `acquire` faceva esattamente questo, implicitamente.
    std::fs::create_dir_all(home.parent().expect("parent")).expect("mkdir");
    std::fs::create_dir(&home).expect("mkdir home");
    let _guard = lockfile::acquire(&lock_path).expect("lock acquisito");

    let ctx = Context {
        odoo_home: home.clone(),
        dry_run: false,
        ..Default::default()
    };
    let mut step =
        PrepareOptRoot::with_ops(Box::new(odoo_installer::system_ops::RealSystemOps::debian()));
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

/// Il logger non deve creare la home: alla prima installazione `/opt/odoo` non
/// esiste ancora, ed è per questo che A-R5-2 lasciava l'utente senza
/// post-mortem su file. Con il log in `/var/log` il file nasce **subito**.
#[test]
fn log_does_not_depend_on_a_directory_the_installer_must_still_create() {
    let root = tempfile::tempdir().expect("tempdir");
    let home: PathBuf = root.path().join("odoo"); // assente, come su macchina vergine
    let var_log = root.path().join("var-log");
    std::fs::create_dir(&var_log).expect("mkdir var-log");

    // Un log dentro la home non nasce (è il vecchio comportamento)…
    assert!(
        logging::try_open(&home.join(".installer.log")).is_none(),
        "senza la home, un log al suo interno non può nascere"
    );
    // …uno fuori sì, e senza far comparire la home.
    assert!(logging::try_open(&var_log.join("odoo-installer.log")).is_some());
    assert!(!home.exists(), "aprire il log non deve creare la home");
}
