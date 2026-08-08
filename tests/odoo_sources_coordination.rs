//! Coordinamento sorgenti/venv (Fase 6): al rollback venv e sorgenti spariscono,
//! e il contenitore install_dir è gestito senza doppie rimozioni.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{ops_of, MockConfig, MockSystemOps, Op, OpLog};
use invok::context::Context;
use invok::engine::Installer;
use invok::step::Step;
use invok::steps::clone_odoo_repo::CloneOdooRepo;
use invok::steps::create_virtualenv::CreateVirtualenv;
use invok::steps::noop::NoopStep;
use invok::system_ops::OdooSourceState;

#[test]
fn rollback_removes_venv_then_sources_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let install_dir = PathBuf::from("/opt/odoo/odoo18");
    let repo_dir = install_dir.join("odoo");
    let venv_dir = install_dir.join("sandbox");

    let log: OpLog = Arc::new(Mutex::new(Vec::new()));

    let clone_ops = MockSystemOps::with_log(
        MockConfig {
            source_state: OdooSourceState::Absent,
            dir_empty: true,
            ..Default::default()
        },
        Arc::clone(&log),
    );
    let venv_ops = MockSystemOps::with_log(
        MockConfig {
            venv_exists: false,
            venv_available: true,
            ..Default::default()
        },
        Arc::clone(&log),
    );

    // Ordine di produzione: clone → venv → (step che fallisce).
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(CloneOdooRepo::with_ops(Box::new(clone_ops))),
        Box::new(CreateVirtualenv::with_ops(Box::new(venv_ops))),
        Box::new(NoopStep::new("boom").fail_on_run()),
    ];

    let ctx = Context {
        odoo_user: "odoo".to_string(),
        odoo_version: "18.0".to_string(),
        install_dir: install_dir.clone(),
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"));

    let mut installer = Installer::new();
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "lo step finale innesca il rollback"
    );

    let ops = ops_of(&log);
    let rm_venv = ops
        .iter()
        .position(|o| matches!(o, Op::RemoveDirAll(p) if *p == venv_dir));
    let rm_repo = ops
        .iter()
        .position(|o| matches!(o, Op::RemoveDirAll(p) if *p == repo_dir));

    assert!(
        rm_venv.is_some(),
        "il venv creato da noi deve essere rimosso"
    );
    assert!(
        rm_repo.is_some(),
        "i sorgenti creati da noi devono essere rimossi"
    );
    assert!(
        rm_venv < rm_repo,
        "l'undo del venv precede quello dei sorgenti (ordine inverso)"
    );

    // Il contenitore install_dir viene rimosso al massimo una volta (rmdir),
    // senza doppie rimozioni conflittuali.
    let container_removals = ops
        .iter()
        .filter(|o| matches!(o, Op::Rmdir(p) if *p == install_dir))
        .count();
    assert!(
        container_removals <= 1,
        "nessuna doppia rimozione del contenitore"
    );
}
