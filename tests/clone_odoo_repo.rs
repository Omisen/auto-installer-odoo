//! Test di [`CloneOdooRepo`] (Fase 6): stato sorgenti, retry, fallback tarball.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::clone_odoo_repo::CloneOdooRepo;
use odoo_installer::system_ops::OdooSourceState;

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_version: "18.0".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn repo_dir() -> PathBuf {
    PathBuf::from("/opt/odoo/odoo18/odoo")
}

fn count(ops: &[Op], pred: impl Fn(&Op) -> bool) -> usize {
    ops.iter().filter(|o| pred(o)).count()
}

#[test]
fn absent_clones_and_undo_removes_target() {
    let cfg = MockConfig {
        source_state: OdooSourceState::Absent,
        dir_empty: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // Clone con gli argomenti attesi.
    assert!(ops.contains(&Op::GitClone {
        target: repo_dir(),
        branch: "18.0".to_string(),
        depth: 5,
    }));
    // Struttura directory creata come utente odoo.
    assert!(ops.iter().any(|o| matches!(o, Op::MkdirAsUser { user, .. } if user == "odoo")));
    // Undo: rm -rf del target (nostro perimetro).
    assert!(ops.contains(&Op::RemoveDirAll(repo_dir())));
}

#[test]
fn git_existing_correct_branch_is_noop() {
    let cfg = MockConfig {
        source_state: OdooSourceState::GitRepo {
            branch: "18.0".to_string(),
        },
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(!ops.iter().any(|o| matches!(o, Op::GitClone { .. })), "già clonato: nessun clone");
    assert!(!ops.iter().any(|o| matches!(o, Op::RemoveDirAll(_))), "Preexisting: undo no-op");
}

#[test]
fn branch_mismatch_is_error_not_regenerated() {
    let cfg = MockConfig {
        source_state: OdooSourceState::GitRepo {
            branch: "17.0".to_string(),
        },
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    // Branch diverso → errore in snapshot, NON si rigenera silenziosamente.
    assert!(step.snapshot(&c).is_err(), "branch mismatch deve essere un errore");
}

#[test]
fn retries_then_succeeds_with_cleanup_between() {
    // Clone fallisce 2 volte poi riesce → 3 tentativi, 2 pulizie tra i tentativi.
    let cfg = MockConfig {
        source_state: OdooSourceState::Absent,
        git_clone_fail_times: 2,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run"); // NON chiamo undo, per isolare i conteggi

    let ops = ops_of(&log);
    assert_eq!(count(&ops, |o| matches!(o, Op::GitClone { .. })), 3, "3 tentativi di clone");
    assert_eq!(
        count(&ops, |o| matches!(o, Op::RemoveDirAll(p) if *p == repo_dir())),
        2,
        "pulizia degli artefatti parziali tra i tentativi"
    );
}

#[test]
fn falls_back_to_tarball_after_all_retries() {
    // Tutti i tentativi di clone falliscono → scatta il fallback tarball.
    let cfg = MockConfig {
        source_state: OdooSourceState::Absent,
        git_clone_fail_times: 3,
        tarball_fails: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run con fallback tarball");

    assert!(ops_of(&log).iter().any(|o| matches!(o, Op::TarballInstall { .. })));
}

#[test]
fn tarball_failure_after_clone_failure_errors() {
    let cfg = MockConfig {
        source_state: OdooSourceState::Absent,
        git_clone_fail_times: 3,
        tarball_fails: true,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = CloneOdooRepo::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "clone + tarball entrambi falliti → errore");
}
