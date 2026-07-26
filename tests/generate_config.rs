//! Test di [`GenerateConfig`] (Fase 7): undo ripristinante + rendering.

mod common;

use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::secret::Secret;
use odoo_installer::step::Step;
use odoo_installer::steps::generate_config::{
    normalize_empty_directives, render_config, validate_rendered,
};
use odoo_installer::steps::generate_config::GenerateConfig;

fn ctx(install_dir: PathBuf) -> Context {
    Context {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        db_password: Secret::default(), // vuota → db_password = False
        port: 8069,
        odoo_home: PathBuf::from("/opt/odoo"),
        install_dir,
        admin_passwd: Secret::new("s3cret"),
        odoo_logfile: None, // → logfile = False
        with_nginx: false,
        dry_run: false,
        ..Default::default()
    }
}

fn dest_of(dir: &Path) -> PathBuf {
    dir.join("odoo18.conf")
}

#[test]
fn created_by_us_generates_640_and_undo_removes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = MockConfig {
        real_fs: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = GenerateConfig::with_ops(Box::new(mock));
    let c = ctx(dir.path().to_path_buf());
    let dest = dest_of(dir.path());

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    assert!(dest.exists(), "il file deve essere creato");
    let content = std::fs::read_to_string(&dest).expect("read");
    assert!(!content.contains("${"), "nessun placeholder residuo");
    assert!(content.contains("[options]"));
    assert!(content.contains("http_port = 8069"));
    assert!(content.contains("db_host = False"), "direttiva vuota → False");

    let ops = ops_of(&log);
    assert!(ops.iter().any(|o| matches!(o, Op::WritePrivateFile(_))), "scrittura privata (600)");
    assert!(ops.iter().any(|o| matches!(o, Op::Chmod { mode, .. } if *mode == 0o640)));
    assert!(ops.iter().any(|o| matches!(o, Op::ChownNamed { owner, .. } if owner == "odoo")));

    step.undo(&c).expect("undo");
    assert!(!dest.exists(), "undo: il file creato da noi viene rimosso");
}

#[test]
fn preexisting_undo_restores_backup() {
    // IL test nuovo: il file originale del cliente viene RIPRISTINATO, non
    // cancellato né lasciato sovrascritto.
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dest_of(dir.path());
    let original = "; CONFIG ORIGINALE DEL CLIENTE\n";
    std::fs::write(&dest, original).expect("write original");

    let cfg = MockConfig {
        real_fs: true,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = GenerateConfig::with_ops(Box::new(mock));
    let c = ctx(dir.path().to_path_buf());

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    // Dopo il run il file è la nostra config (originale messo da parte nel backup).
    let after_run = std::fs::read_to_string(&dest).expect("read");
    assert_ne!(after_run, original, "il run sovrascrive con la config generata");

    step.undo(&c).expect("undo");

    // Dopo l'undo il file torna ESATTAMENTE l'originale del cliente.
    let after_undo = std::fs::read_to_string(&dest).expect("read");
    assert_eq!(after_undo, original, "undo deve ripristinare il file originale dal backup");
}

#[test]
fn rendering_normalizes_empty_directives_and_no_residue() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = ctx(dir.path().to_path_buf());
    let tpl = "[options]\n\
               addons_path = ${ODOO_ADDONS_PATH}\n\
               http_port = ${ODOO_PORT}\n\
               db_host = ${DB_HOST}\n\
               db_password = ${DB_PASSWORD}\n\
               logfile = ${ODOO_LOGFILE}\n\
               admin_passwd = ${ODOO_ADMIN_PASSWD}\n";
    let content = render_config(tpl, &c);

    assert!(!content.contains("${"), "nessun placeholder residuo");
    assert!(content.contains("http_port = 8069"));
    assert!(content.contains("db_host = False"), "db_host vuoto → False");
    assert!(content.contains("db_password = False"), "db_password vuota → False");
    assert!(content.contains("logfile = False"), "logfile assente → False");
    assert!(content.contains("admin_passwd = s3cret"), "la password è nel file (non nei log)");
    validate_rendered(&content).expect("valido");
}

#[test]
fn validation_rejects_residue_and_missing_section() {
    assert!(validate_rendered("[options]\naddons_path = /x\nhttp_port = 8069\nfoo = ${BAR}\n").is_err());
    assert!(validate_rendered("addons_path = /x\nhttp_port = 8069\n").is_err()); // no [options]
    assert!(validate_rendered("[options]\nhttp_port = 8069\n").is_err()); // no addons_path
    assert!(validate_rendered("[options]\naddons_path = /x\nhttp_port = 8069\n").is_ok());
}

#[test]
fn normalize_directive_helper() {
    assert_eq!(normalize_empty_directives("db_host = \n"), "db_host = False\n");
    assert_eq!(normalize_empty_directives("key = value\n"), "key = value\n");
    assert_eq!(normalize_empty_directives("; comment\n"), "; comment\n");
}
