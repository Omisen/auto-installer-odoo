//! Test di [`SetupPostgres`] (Fase 5): i tre assi indipendenti e la D4.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::setup_postgres::SetupPostgres;

fn ctx(aggressive: bool) -> Context {
    Context {
        dry_run: false,
        aggressive_rollback: aggressive,
        ..Default::default()
    }
}

fn installed(pkgs: &[&str]) -> HashSet<String> {
    pkgs.iter().map(|s| s.to_string()).collect()
}

fn run_cycle(cfg: MockConfig, aggressive: bool) -> Vec<Op> {
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupPostgres::with_ops(Box::new(mock));
    let c = ctx(aggressive);
    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    ops_of(&log)
}

fn has(ops: &[Op], pred: impl Fn(&Op) -> bool) -> bool {
    ops.iter().any(pred)
}

#[test]
fn installed_but_stopped_starts_then_stops_no_purge() {
    // PostgreSQL installato + enabled, ma fermo. Lo avviamo; l'undo lo ferma
    // (D4) ma NON disabilita (era già enabled) e NON purga.
    let cfg = MockConfig {
        installed_packages: installed(&["postgresql"]),
        service_enabled: true,
        service_active: false,
        ..Default::default()
    };
    let ops = run_cycle(cfg, false);

    assert!(has(&ops, |o| matches!(o, Op::ServiceStart(_))), "deve avviare");
    assert!(has(&ops, |o| matches!(o, Op::ServiceStop(_))), "undo deve fermare (D4)");
    assert!(!has(&ops, |o| matches!(o, Op::ServiceDisable(_))), "non disabilitare: era già enabled");
    assert!(!has(&ops, |o| matches!(o, Op::AptInstall(_))), "già installato: non installare");
    assert!(!has(&ops, |o| matches!(o, Op::AptPurge(_))), "mai purgare senza --aggressive-rollback");
}

#[test]
fn all_absent_installs_enables_starts_then_reverts_no_purge() {
    let cfg = MockConfig::default(); // niente installato, disabled, fermo
    let ops = run_cycle(cfg, false);

    assert!(has(&ops, |o| matches!(o, Op::AptInstall(_))));
    assert!(has(&ops, |o| matches!(o, Op::ServiceEnable(_))));
    assert!(has(&ops, |o| matches!(o, Op::ServiceStart(_))));
    // undo: stop + disable, ma NO purge (default).
    assert!(has(&ops, |o| matches!(o, Op::ServiceStop(_))));
    assert!(has(&ops, |o| matches!(o, Op::ServiceDisable(_))));
    assert!(!has(&ops, |o| matches!(o, Op::AptPurge(_))), "no purge senza flag");
}

#[test]
fn purge_only_with_aggressive_rollback() {
    // Stesso stato (tutto assente → installed CreatedByUs), due politiche.
    let without = run_cycle(MockConfig::default(), false);
    assert!(!has(&without, |o| matches!(o, Op::AptPurge(_))), "senza flag: no purge");

    let with = run_cycle(MockConfig::default(), true);
    assert!(has(&with, |o| matches!(o, Op::AptPurge(_))), "con --aggressive-rollback: purga");
    assert!(has(&with, |o| matches!(o, Op::AptAutoremove)));
}

#[test]
fn already_active_is_left_running() {
    // Era già attivo (Preexisting) → l'undo NON deve fermarlo (D4).
    let cfg = MockConfig {
        installed_packages: installed(&["postgresql"]),
        service_enabled: true,
        service_active: true,
        ..Default::default()
    };
    let ops = run_cycle(cfg, false);

    assert!(!has(&ops, |o| matches!(o, Op::ServiceStop(_))), "un servizio già attivo va lasciato running");
    assert!(!has(&ops, |o| matches!(o, Op::ServiceStart(_))), "già attivo: nessuno start");
}
