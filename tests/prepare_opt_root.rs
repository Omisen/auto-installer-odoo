//! Test del primo step reale (Fase 2): ciclo `snapshot → run → undo` di
//! [`PrepareOptRoot`] contro il filesystem reale (in tempdir, senza root).

use std::path::PathBuf;

use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::prepare_opt_root::PrepareOptRoot;

/// Context minimale: al passo servono solo `odoo_home` e `dry_run`.
fn ctx(home: PathBuf, dry_run: bool) -> Context {
    Context {
        odoo_home: home,
        dry_run,
        ..Default::default()
    }
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
    let mut step = PrepareOptRoot::new();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(home.exists(), "run deve creare la directory");
    assert_eq!(persisted_prestate(&step), PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert!(!home.exists(), "undo deve rimuovere la directory creata da noi");
}

#[test]
fn preexisting_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf(); // esiste già
    assert!(home.exists());

    let c = ctx(home.clone(), false);
    let mut step = PrepareOptRoot::new();

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
    let mut step = PrepareOptRoot::new();

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
    let mut step = PrepareOptRoot::new();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // In dry-run non si crea nulla e lo stato resta Untracked (undo NO-OP).
    assert!(!home.exists(), "dry-run non deve creare la directory");
    assert_eq!(persisted_prestate(&step), PreState::Untracked);

    step.undo(&c).expect("undo");
    assert!(!home.exists());
}
