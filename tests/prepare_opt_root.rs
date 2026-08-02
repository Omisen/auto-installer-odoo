//! Test del primo step reale (Fase 2): ciclo `snapshot → run → undo` di
//! [`PrepareOptRoot`] contro il filesystem reale (in tempdir, senza root).
//!
//! La directory si crea e si rimuove davvero, in una tempdir; utente e `chown`
//! passano invece da un mock. Non è un dettaglio: dopo A-V3-4 lo step chiede al
//! sistema se l'utente esiste, e con `SystemOps` reale l'esito dei test
//! dipenderebbe dalla macchina che li esegue.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::prepare_opt_root::PrepareOptRoot;

/// Context minimale: al passo servono `odoo_home`, `odoo_user` e `dry_run`.
fn ctx(home: PathBuf, dry_run: bool) -> Context {
    Context {
        odoo_home: home,
        odoo_user: "odoo".to_string(),
        dry_run,
        ..Default::default()
    }
}

/// Step con utente **assente**: il caso normale, in cui la home resta a root in
/// attesa di `CreateOdooUser`.
fn step_without_user() -> PrepareOptRoot {
    let (mock, _log) = MockSystemOps::new(MockConfig::default());
    PrepareOptRoot::with_ops(Box::new(mock))
}

/// Legge il `PreState` persistito dallo step (via `snapshot_value`).
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
    assert!(home.exists(), "run deve creare la directory");
    assert_eq!(persisted_prestate(&step), PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert!(
        !home.exists(),
        "undo deve rimuovere la directory creata da noi"
    );
}

#[test]
fn preexisting_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf(); // esiste già
    assert!(home.exists());

    let c = ctx(home.clone(), false);
    let mut step = step_without_user();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted_prestate(&step), PreState::Preexisting);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    // La directory preesistente sopravvive: non è nostra, non la cancelliamo.
    assert!(home.exists(), "undo NON deve rimuovere una dir Preexisting");
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

    // Simula un artefatto di uno step successivo dentro la dir.
    std::fs::write(home.join("intruso.txt"), b"x").expect("write file");

    // undo è best-effort: logga e NON rimuove (niente rm -rf).
    step.undo(&c).expect("undo best-effort");
    assert!(
        home.exists(),
        "undo non deve rimuovere una dir non vuota (no rm -rf)"
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

    // In dry-run non si crea nulla e lo stato resta Untracked (undo NO-OP).
    assert!(!home.exists(), "dry-run non deve creare la directory");
    assert_eq!(persisted_prestate(&step), PreState::Untracked);

    step.undo(&c).expect("undo");
    assert!(!home.exists());
}

// --- A-V3-4: la consegna della home quando l'utente esiste già ---------------

/// Utente `odoo` già presente sulla macchina: la home appena creata gli viene
/// consegnata **subito**, qui.
///
/// `owned root` è una condizione d'attesa, non lo stato giusto della home: ha
/// senso solo finché l'utente non esiste. Se esiste, nessuno gliela consegnerà
/// più — `CreateOdooUser` vede l'utente `Preexisting` e ritorna senza toccare
/// nulla — e l'installazione muore tre step dopo, su un `mkdir` come `odoo`
/// dentro una directory di root.
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

    assert!(home.exists(), "la directory va comunque creata");
    assert_eq!(
        persisted_prestate(&step),
        PreState::CreatedByUs,
        "la consegna non cambia la proprietà: la directory resta nostra da rimuovere"
    );

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Op::ChownNamed { path, owner, group }
                if *path == home && owner == "odoo" && group == "odoo"
        )),
        "la home deve essere consegnata all'utente che esiste già: {ops:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::Chmod { path, mode } if *path == home && *mode == 0o750)),
        "stessi permessi che imposterebbe CreateOdooUser: la home deve risultare \
         identica quale che sia lo step che l'ha consegnata: {ops:?}"
    );
}

/// Senza utente non si consegna niente: è il caso normale, e la home resta a
/// root finché `CreateOdooUser` non crea l'utente e fa il `chown` lui.
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
        "nessuna consegna: l'utente non esiste ancora, ci penserà CreateOdooUser: {ops:?}"
    );
}

/// Una directory **preesistente** non si consegna a nessuno, nemmeno se l'utente
/// esiste: non è nostra. È il confine fra questo fix e l'anti-drop applicato
/// alle directory — la proprietà dell'artefatto decide, non la comodità.
#[test]
fn a_preexisting_home_is_never_handed_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf(); // esiste già

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
        "su una directory non nostra non si tocca nulla: {:?}",
        ops_of(&log)
    );
}

/// In dry-run non si crea e non si consegna.
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
    assert!(ops_of(&log).is_empty(), "dry-run non muta nulla");
}
