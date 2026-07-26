//! Test di [`SetupSystemd`] (Fase 8): tre assi (D4) + ordine undo + rendering.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::setup_systemd::{render_unit, validate_unit, SetupSystemd};

fn ctx() -> Context {
    Context {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        odoo_home: std::path::PathBuf::from("/opt/odoo"),
        install_dir: std::path::PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn pos(ops: &[Op], pred: impl Fn(&Op) -> bool) -> Option<usize> {
    ops.iter().position(pred)
}
fn has(ops: &[Op], pred: impl Fn(&Op) -> bool) -> bool {
    ops.iter().any(pred)
}

#[test]
fn all_absent_installs_then_undo_in_order() {
    let cfg = MockConfig::default(); // unit assente, disabled, fermo
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);

    // run: installa + enable + start.
    assert!(has(&ops, |o| matches!(o, Op::WritePrivateFile(_))));
    assert!(has(&ops, |o| matches!(o, Op::Chmod { mode, .. } if *mode == 0o644)));
    assert!(has(&ops, |o| matches!(o, Op::ChownNamed { owner, .. } if owner == "root")));
    assert!(has(&ops, |o| matches!(o, Op::ServiceEnable(_))));
    assert!(has(&ops, |o| matches!(o, Op::ServiceStart(_))));

    // undo: ordine stop → disable → rm → reload.
    let stop = pos(&ops, |o| matches!(o, Op::ServiceStop(_))).expect("stop");
    let disable = pos(&ops, |o| matches!(o, Op::ServiceDisable(_))).expect("disable");
    let rm = pos(&ops, |o| matches!(o, Op::RemoveFile(_))).expect("rm");
    // l'ultimo daemon-reload è quello dell'undo.
    let reload = ops
        .iter()
        .rposition(|o| matches!(o, Op::DaemonReload))
        .expect("reload");

    assert!(stop < disable, "stop prima di disable");
    assert!(disable < rm, "disable prima di rm");
    assert!(rm < reload, "rm prima del daemon-reload finale");
}

#[test]
fn d4_active_already_running_not_stopped() {
    let cfg = MockConfig {
        service_active: true, // già attivo (Preexisting)
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // Già attivo → restart in run (non start), e undo NON ferma (D4).
    assert!(has(&ops, |o| matches!(o, Op::ServiceRestart(_))));
    assert!(!has(&ops, |o| matches!(o, Op::ServiceStop(_))), "un servizio già attivo va lasciato running");
}

#[test]
fn d4_enabled_already_enabled_not_disabled() {
    let cfg = MockConfig {
        service_enabled: true, // già enabled (Preexisting)
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(!has(&ops, |o| matches!(o, Op::ServiceEnable(_))), "già enabled: nessun enable");
    assert!(!has(&ops, |o| matches!(o, Op::ServiceDisable(_))), "già enabled: nessun disable in undo (D4)");
}

#[test]
fn start_failure_is_error() {
    let cfg = MockConfig {
        service_start_fails: true, // il servizio non diventa attivo
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "se il servizio non parte, il run deve fallire");
}

#[test]
fn rendering_has_no_residue_and_keeps_hardening() {
    let c = ctx();
    let unit = render_unit(&c);

    assert!(!unit.contains("{{"), "nessun placeholder residuo");
    validate_unit(&unit).expect("unit valido");

    // Hardening preservato.
    assert!(unit.contains("User=odoo"));
    assert!(unit.contains("Group=odoo"));
    assert!(unit.contains("NoNewPrivileges=true"));
    assert!(unit.contains("PrivateTmp=true"));
    assert!(unit.contains("RuntimeDirectory=odoo"));
    assert!(unit.contains("Requires=postgresql.service"));
    // Percorsi renderizzati.
    assert!(unit.contains("/opt/odoo/odoo18/sandbox/bin/python3"));
    assert!(unit.contains("/opt/odoo/odoo18/odoo/odoo-bin"));
    assert!(unit.contains("/opt/odoo/odoo18/odoo18.conf"));
}
