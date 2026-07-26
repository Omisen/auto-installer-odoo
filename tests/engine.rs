//! Test end-to-end del motore contro le 4 invarianti di `CLAUDE.md`.
//!
//! Non toccano il sistema: usano `NoopStep` e una directory temporanea per il
//! file di stato, quindi girano senza root.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::state::{InstallState, PreState, StepRecord};
use odoo_installer::step::Step;
use odoo_installer::steps::noop::{NoopStep, UndoLog};

/// Context non-dry con file di stato in una tempdir (niente root, niente `/opt`).
/// Il motore usa solo `dry_run` e `state_path`; il resto è irrilevante qui.
fn ctx_with_state(dir: &tempfile::TempDir) -> Context {
    Context {
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join(".installer-state.json"))
}

/// Invariante 2: il rollback esegue gli `undo` dall'ultimo completato al primo.
#[test]
fn rollback_runs_undo_in_reverse_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // alpha, beta completano; gamma fallisce al run e innesca il rollback.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("beta").with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("gamma").fail_on_run().with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // Solo alpha e beta erano completati; gamma non ha completato → niente undo.
    // Ordine inverso: prima beta, poi alpha.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
}

/// Invariante 3: un `undo` che fallisce non impedisce agli altri di eseguire.
#[test]
fn rollback_is_best_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // beta fallirà l'undo; alpha deve comunque essere ripulito dopo.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("beta").fail_on_undo().with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("gamma").fail_on_run().with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // beta agisce (e fallisce), ma alpha viene comunque ripulito dopo di lui.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
}

/// Invariante 3 / protezione: uno step `Preexisting` non compie azioni di undo,
/// pur essendo `undo` invocato dal motore.
#[test]
fn preexisting_step_undo_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // alpha è preesistente (non nostro): undo deve essere NO-OP.
    let alpha = NoopStep::new("alpha")
        .preexisting()
        .with_undo_log(Arc::clone(&log));
    let alpha_calls = alpha.undo_call_handle();

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(alpha),
        Box::new(NoopStep::new("beta").fail_on_run().with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "l'esecuzione deve fallire su beta");
    // undo è stato invocato su alpha...
    assert_eq!(
        alpha_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "undo di alpha deve essere invocato dal motore"
    );
    // ...ma non ha compiuto alcuna azione (log vuoto: nessun artefatto nostro).
    assert!(
        log.lock().expect("lock").is_empty(),
        "un undo su Preexisting non deve compiere azioni"
    );
}

/// Invariante 4: lo stato scritto e riletto mantiene i `completed`; permessi 0600.
#[test]
fn install_state_roundtrip_and_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".installer-state.json");

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });
    state.record(StepRecord {
        name: "beta".to_string(),
        snapshot: serde_json::to_value(PreState::Preexisting).expect("serialize"),
    });

    state.save(&path).expect("save");

    // Permessi 0600.
    let mode = std::fs::metadata(&path).expect("metadata").permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "il file di stato deve essere 0600");

    // Round-trip: i record sopravvivono al ciclo scrittura/lettura.
    let reloaded = InstallState::load(&path).expect("load");
    assert_eq!(reloaded, state);
    assert_eq!(reloaded.completed.len(), 2);
    assert_eq!(reloaded.completed[0].name, "alpha");
    assert_eq!(reloaded.completed[1].name, "beta");

    // load su file assente → stato vuoto (prima esecuzione), non errore.
    InstallState::clear(&path).expect("clear");
    let empty = InstallState::load(&path).expect("load assente");
    assert!(empty.completed.is_empty());
}

/// Percorso felice: tutti gli step completano, lo stato è persistito su disco.
#[test]
fn successful_run_persists_all_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::new();
    installer.execute(&mut steps, &ctx).expect("esecuzione ok");

    assert_eq!(installer.state().completed.len(), 2);

    let persisted = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(persisted.completed.len(), 2);
    assert_eq!(persisted.completed[0].name, "alpha");
    assert_eq!(persisted.completed[1].name, "beta");
}
