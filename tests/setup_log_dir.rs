//! Test di [`SetupLogDir`] (Fase 3): disabilitato → no-op; abilitato → crea/rimuove.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::setup_log_dir::SetupLogDir;

fn ctx(logfile: Option<PathBuf>) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_logfile: logfile,
        dry_run: false,
        ..Default::default()
    }
}

fn persisted(step: &SetupLogDir) -> PreState {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

#[test]
fn disabled_logfile_is_full_noop() {
    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupLogDir::with_ops(Box::new(mock));
    let c = ctx(None); // ODOO_LOGFILE assente

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step), PreState::Untracked);
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(ops_of(&log).is_empty(), "logfile disabilitato → nessuna azione");
}

#[test]
fn created_by_us_creates_and_removes() {
    // Dir del logfile inesistente → la creiamo, poi la rimuoviamo (vuota).
    let cfg = MockConfig {
        path_exists: false,
        dir_empty: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupLogDir::with_ops(Box::new(mock));
    let logfile = PathBuf::from("/var/log/odoo/odoo.log");
    let dir = PathBuf::from("/var/log/odoo");
    let c = ctx(Some(logfile));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert_eq!(persisted(&step), PreState::CreatedByUs);
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.contains(&Op::Mkdir(dir.clone())));
    assert!(ops.iter().any(|op| matches!(op, Op::ChownNamed { owner, group, .. } if owner == "odoo" && group == "odoo")));
    assert!(ops.iter().any(|op| matches!(op, Op::Chmod { mode, .. } if *mode == 0o750)));
    assert!(ops.contains(&Op::Rmdir(dir)), "undo deve rimuovere la dir creata (vuota)");
}

#[test]
fn preexisting_dir_is_not_touched() {
    // Dir già esistente: nessuna creazione, e undo non la rimuove.
    let cfg = MockConfig {
        path_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupLogDir::with_ops(Box::new(mock));
    let c = ctx(Some(PathBuf::from("/var/log/odoo/odoo.log")));

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step), PreState::Preexisting);
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(ops_of(&log).is_empty(), "una log dir Preexisting non va toccata");
}

#[test]
fn undo_does_not_remove_non_empty_dir() {
    // Creata da noi ma non vuota (log già scritti) → undo non rimuove.
    let cfg = MockConfig {
        path_exists: false,
        dir_empty: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupLogDir::with_ops(Box::new(mock));
    let c = ctx(Some(PathBuf::from("/var/log/odoo/odoo.log")));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        !ops.iter().any(|op| matches!(op, Op::Rmdir(_))),
        "undo non deve rimuovere una log dir non vuota"
    );
}
