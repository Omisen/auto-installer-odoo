//! Test di [`CreateVirtualenv`] (Fase 6).

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::create_virtualenv::CreateVirtualenv;

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn venv_dir() -> PathBuf {
    PathBuf::from("/opt/odoo/odoo18/sandbox")
}

#[test]
fn absent_creates_and_undo_removes() {
    let cfg = MockConfig {
        venv_exists: false,
        venv_available: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.contains(&Op::CreateVenv(venv_dir())));
    assert!(ops.contains(&Op::RemoveDirAll(venv_dir())), "undo: rm -rf del venv");
}

#[test]
fn preexisting_venv_is_noop() {
    let cfg = MockConfig {
        venv_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(ops_of(&log).is_empty(), "un venv preesistente non va né creato né rimosso");
}

#[test]
fn missing_python_venv_is_error() {
    let cfg = MockConfig {
        venv_exists: false,
        venv_available: false, // python3-venv assente
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "senza python3-venv il run deve fallire");
}
