//! [`SetupSystemd`]: the three axes (D4), the undo's order, and rendering.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::setup_systemd::{render_unit, validate_unit, SetupSystemd};

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

    // run: install, enable, start.
    assert!(has(&ops, |o| matches!(o, Op::CreatePrivateFile(_))));
    assert!(has(
        &ops,
        |o| matches!(o, Op::Chmod { mode, .. } if *mode == 0o644)
    ));
    assert!(has(
        &ops,
        |o| matches!(o, Op::ChownNamed { owner, .. } if owner == "root")
    ));
    assert!(has(&ops, |o| matches!(o, Op::ServiceEnable(_))));
    assert!(has(&ops, |o| matches!(o, Op::ServiceStart(_))));

    // undo, in order: stop → disable → rm → reload.
    let stop = pos(&ops, |o| matches!(o, Op::ServiceStop(_))).expect("stop");
    let disable = pos(&ops, |o| matches!(o, Op::ServiceDisable(_))).expect("disable");
    let rm = pos(&ops, |o| matches!(o, Op::RemoveFile(_))).expect("rm");
    // the last daemon-reload is the undo's.
    let reload = ops
        .iter()
        .rposition(|o| matches!(o, Op::DaemonReload))
        .expect("reload");

    assert!(stop < disable, "stop before disable");
    assert!(disable < rm, "disable before removal");
    assert!(rm < reload, "rm prima del daemon-reload finale");
}

#[test]
fn d4_active_already_running_not_stopped() {
    let cfg = MockConfig {
        service_active: true, // already active (Preexisting)
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // already running: the run restarts rather than starts, and the undo leaves
    // it (D4).
    assert!(has(&ops, |o| matches!(o, Op::ServiceRestart(_))));
    assert!(
        !has(&ops, |o| matches!(o, Op::ServiceStop(_))),
        "a service already active is left running"
    );
}

#[test]
fn d4_enabled_already_enabled_not_disabled() {
    let cfg = MockConfig {
        service_enabled: true, // already enabled (Preexisting)
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupSystemd::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        !has(&ops, |o| matches!(o, Op::ServiceEnable(_))),
        "already enabled: no enable"
    );
    assert!(
        !has(&ops, |o| matches!(o, Op::ServiceDisable(_))),
        "already enabled: no disable in the undo (D4)"
    );
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
    assert!(
        step.run(&c).is_err(),
        "if the service does not start, the run must fail"
    );
}

#[test]
fn rendering_has_no_residue_and_keeps_hardening() {
    let c = ctx();
    let unit = render_unit(&c);

    assert!(!unit.contains("{{"), "nessun placeholder residuo");
    validate_unit(&unit).expect("unit valido");

    // hardening preserved.
    assert!(unit.contains("User=odoo"));
    assert!(unit.contains("Group=odoo"));
    assert!(unit.contains("NoNewPrivileges=true"));
    assert!(unit.contains("PrivateTmp=true"));
    assert!(unit.contains("RuntimeDirectory=odoo"));
    assert!(unit.contains("Requires=postgresql.service"));
    // paths rendered.
    assert!(unit.contains("/opt/odoo/odoo18/sandbox/bin/python3"));
    assert!(unit.contains("/opt/odoo/odoo18/odoo/odoo-bin"));
    assert!(unit.contains("/opt/odoo/odoo18/odoo18.conf"));
}

/// A-V3-13: the "Security hardening" heading covered an **inert** directive.
///
/// `PermissionsStartOnly=true` is deprecated since systemd 231 and ignored with
/// a warning; it made `ExecStartPre` commands run as root, and there are none.
/// nothing under that heading was hardening anything.
#[test]
fn the_unit_makes_no_hollow_hardening_promises() {
    let unit = render_unit(&ctx());

    // the **active** lines, not the text: the comment explaining why the
    // directive was removed names it, rightly.
    let attive: Vec<&str> = unit
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert!(
        !attive.iter().any(|l| l.starts_with("PermissionsStartOnly")),
        "a directive deprecated and ignored by systemd: it must not be active"
    );

    // the directives that actually move the needle for a network-facing
    // process.
    for direttiva in [
        "ProtectSystem=full",
        "ProtectHome=true",
        "PrivateDevices=true",
        "ProtectKernelTunables=true",
        "ProtectKernelModules=true",
        "ProtectControlGroups=true",
        "RestrictSUIDSGID=true",
        "LockPersonality=true",
    ] {
        assert!(unit.contains(direttiva), "manca {direttiva}");
    }

    // AF_UNIX is not optional: it is PostgreSQL's socket. without it the
    // service starts and cannot reach the database.
    let families = attive
        .iter()
        .find(|l| l.starts_with("RestrictAddressFamilies="))
        .expect("RestrictAddressFamilies presente");
    for family in ["AF_UNIX", "AF_INET", "AF_INET6"] {
        assert!(
            families.contains(family),
            "{family} is missing from: {families}"
        );
    }

    // `strict` is deliberately excluded: it needs an exact `ReadWritePaths`
    // list, and getting one wrong breaks the service on a customer machine.
    assert!(
        !attive.iter().any(|l| l.starts_with("ProtectSystem=strict")),
        "ProtectSystem=strict without ReadWritePaths would stop Odoo writing"
    );
}
