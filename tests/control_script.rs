//! Test di [`WriteControlScript`] (Fase 10): ownership SUDO_USER, non globale.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::write_control_script::{control_script_content, WriteControlScript};

fn ctx(sudo_user: Option<&str>) -> Context {
    Context {
        sudo_user: sudo_user.map(|s| s.to_string()),
        odoo_user: "odoo".to_string(),
        odoo_version_short: "18".to_string(),
        dry_run: false,
        ..Default::default()
    }
}

/// Estrae tutti i path referenziati dalle operazioni (per il check "non globale").
fn all_paths(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .flat_map(|o| match o {
            Op::WritePrivateFile(p)
            | Op::RemoveFile(p)
            | Op::RemoveSymlink(p)
            | Op::Rmdir(p)
            | Op::Chmod { path: p, .. } => vec![p.to_string_lossy().into_owned()],
            Op::MkdirAsUser { path, .. } => vec![path.to_string_lossy().into_owned()],
            Op::CreateSymlink { src, link } => {
                vec![
                    src.to_string_lossy().into_owned(),
                    link.to_string_lossy().into_owned(),
                ]
            }
            Op::ChownToUser { path, .. } => vec![path.to_string_lossy().into_owned()],
            _ => vec![],
        })
        .collect()
}

#[test]
fn absent_creates_owned_by_sudo_user_and_undo_removes() {
    let cfg = MockConfig {
        sudo_home: Some("/home/alice".to_string()),
        path_exists: false,
        our_link_exists: false,
        dir_empty: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(Some("alice"));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    // Script + symlink creati.
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::WritePrivateFile(p) if p.ends_with(".scripts/odoo.sh"))));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::CreateSymlink { link, .. } if link.ends_with(".local/bin/odoo"))));

    // Ownership: SUDO_USER (alice), MAI odoo o root.
    let chowns: Vec<&String> = ops
        .iter()
        .filter_map(|o| match o {
            Op::ChownToUser { user, .. } => Some(user),
            _ => None,
        })
        .collect();
    assert!(!chowns.is_empty());
    assert!(
        chowns.iter().all(|u| *u == "alice"),
        "owner deve essere SUDO_USER, trovato: {chowns:?}"
    );

    // Non globale: nulla in /usr/local/bin o path di sistema.
    assert!(
        all_paths(&ops).iter().all(|p| !p.contains("/usr/")),
        "il comando non va installato globalmente"
    );

    // Undo rimuove i nostri artefatti.
    step.undo(&c).expect("undo");
    let ops = ops_of(&log);
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveSymlink(p) if p.ends_with(".local/bin/odoo"))));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::RemoveFile(p) if p.ends_with(".scripts/odoo.sh"))));
}

#[test]
fn preexisting_artifacts_are_not_recreated_or_removed() {
    let cfg = MockConfig {
        sudo_home: Some("/home/alice".to_string()),
        path_exists: true,
        our_link_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(Some("alice"));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);

    // A-V3-9: lo script si riscrive SEMPRE. Il suo contenuto lo generiamo noi e
    // porta dentro il nome del servizio: saltarlo perché "esiste già" lasciava
    // l'helper `odoo` a pilotare il servizio di un'installazione precedente
    // (`SERVICE_NAME=odoo17` mentre gira `odoo18`).
    assert!(
        ops.iter().any(|o| matches!(o, Op::WritePrivateFile(_))),
        "lo script va riscritto: il suo contenuto dipende dalla versione installata"
    );
    // Ma non si distrugge quello che c'era: prima si mette da parte.
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::CopyFile { src, .. } if src.ends_with(".scripts/odoo.sh"))),
        "uno script preesistente va salvato in un backup prima di riscriverlo: {ops:?}"
    );

    assert!(
        !ops.iter().any(|o| matches!(o, Op::CreateSymlink { .. })),
        "symlink preesistente: non ricreato (punta comunque allo stesso script)"
    );
    // L'undo non rimuove ciò che non era nostro: lo rimette com'era.
    assert!(
        !ops.iter().any(|o| matches!(o, Op::RemoveFile(_))),
        "non rimuoviamo artefatti non nostri"
    );
    assert!(!ops.iter().any(|o| matches!(o, Op::RemoveSymlink(_))));
    assert!(
        ops.iter()
            .any(|o| matches!(o, Op::MoveFile { dst, .. } if dst.ends_with(".scripts/odoo.sh"))),
        "l'undo deve rimettere al suo posto lo script preesistente: {ops:?}"
    );
}

#[test]
fn missing_sudo_user_is_error() {
    let (mock, _log) = MockSystemOps::new(MockConfig::default());
    let mut step = WriteControlScript::with_ops(Box::new(mock));
    let c = ctx(None); // SUDO_USER assente
    assert!(
        step.snapshot(&c).is_err(),
        "senza SUDO_USER lo step deve fallire"
    );
}

#[test]
fn script_content_wraps_service_and_user() {
    let content = control_script_content("odoo18", "odoo");
    assert!(content.contains("SERVICE_NAME=\"odoo18\""));
    assert!(content.contains("ODOO_OS_USER=\"odoo\""));
    assert!(content.contains("systemctl start"));
    assert!(content.contains("systemctl status"));
}
