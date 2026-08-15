//! [`GenerateConfig`]: the restoring undo, and the rendering.

mod common;

use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::secret::Secret;
use invok::step::Step;
use invok::steps::generate_config::GenerateConfig;
use invok::steps::generate_config::{
    data_dir, normalize_empty_directives, render_config, validate_rendered,
};

/// the shipped template, read the same way the step reads it: a copy written
/// into the test would be a second source that can drift.
const TEMPLATE: &str = include_str!("../templates/odoo.conf.tpl");

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

    assert!(dest.exists(), "the file must be created");
    let content = std::fs::read_to_string(&dest).expect("read");
    assert!(!content.contains("${"), "nessun placeholder residuo");
    assert!(content.contains("[options]"));
    assert!(content.contains("http_port = 8069"));
    assert!(
        content.contains("db_host = False"),
        "direttiva vuota → False"
    );

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|o| matches!(o, Op::CreatePrivateFile(_))),
        "the temporary is created privately (0600, O_EXCL|O_NOFOLLOW)"
    );
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::Chmod { mode, .. } if *mode == 0o640)));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Op::ChownNamed { owner, .. } if owner == "odoo")));

    step.undo(&c).expect("undo");
    assert!(!dest.exists(), "undo: the file we created is removed");
}

#[test]
fn preexisting_undo_restores_backup() {
    // THE test: the customer's original file is RESTORED, neither deleted nor
    // left overwritten.
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

    // after the run the file is ours, the original set aside in the backup.
    let after_run = std::fs::read_to_string(&dest).expect("read");
    assert_ne!(
        after_run, original,
        "the run overwrites with the generated config"
    );

    step.undo(&c).expect("undo");

    // after the undo it is EXACTLY the customer's original again.
    let after_undo = std::fs::read_to_string(&dest).expect("read");
    assert_eq!(
        after_undo, original,
        "the undo must restore the original file from the backup"
    );
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
    assert!(
        content.contains("db_password = False"),
        "db_password vuota → False"
    );
    assert!(
        content.contains("logfile = False"),
        "logfile assente → False"
    );
    assert!(
        content.contains("admin_passwd = s3cret"),
        "the password is in the file, not in the logs"
    );
    validate_rendered(&content).expect("valido");
}

#[test]
fn validation_rejects_residue_and_missing_section() {
    assert!(
        validate_rendered("[options]\naddons_path = /x\nhttp_port = 8069\nfoo = ${BAR}\n").is_err()
    );
    assert!(validate_rendered("addons_path = /x\nhttp_port = 8069\n").is_err()); // no [options]
    assert!(validate_rendered("[options]\nhttp_port = 8069\n").is_err()); // no addons_path
    assert!(validate_rendered("[options]\naddons_path = /x\nhttp_port = 8069\n").is_ok());
}

#[test]
fn normalize_directive_helper() {
    assert_eq!(
        normalize_empty_directives("db_host = \n"),
        "db_host = False\n"
    );
    assert_eq!(normalize_empty_directives("key = value\n"), "key = value\n");
    assert_eq!(normalize_empty_directives("; comment\n"), "; comment\n");
}

// --- I3: the ports and the filestore in the rendered config -----------------

/// the config must carry **this** instance's gevent port. `gevent_port = 8072`
/// was written verbatim by every installation, so two instances agreed to fight
/// over one socket — and the loser's failure is at *startup*, long after the
/// installation reported success.
#[test]
fn the_rendered_config_carries_this_instances_gevent_port() {
    let mut c = ctx(PathBuf::from("/opt/odoo/odoo-cliente-x"));
    c.instance = Some("cliente-x".to_string());
    c.port = 8169;
    c.gevent_port = 8172;

    let rendered = render_config(TEMPLATE, &c);
    let directive = rendered
        .lines()
        .find(|l| l.trim_start().starts_with("gevent_port"))
        .expect("the config must declare a gevent port");
    assert_eq!(directive.trim(), "gevent_port = 8172");
    assert!(
        rendered.lines().any(|l| l.trim() == "http_port = 8169"),
        "and the HTTP port stays the one that was chosen"
    );
}

/// § 4.1, the filestore: two instances must never write attachments into one
/// directory. `data_dir` derives from the **user's home**, which (α) made
/// per-instance — this is the test that keeps it that way, because the
/// derivation is one line and the damage is silent.
///
/// silent because Odoo keeps attachments in `filestore/<db>/`, so the files do
/// not mix: what collides is **ownership**. `setup-data-dir`'s undo removes the
/// highest level that was missing, so rolling back whoever created it first
/// would carry off everybody's filestore.
#[test]
fn no_two_instances_share_a_filestore() {
    let unnamed = ctx(PathBuf::from("/opt/odoo/odoo18"));

    let mut named = ctx(PathBuf::from("/opt/odoo/odoo-cliente-x"));
    named.instance = Some("cliente-x".to_string());

    let mut other = ctx(PathBuf::from("/opt/odoo/odoo-cliente-y"));
    other.instance = Some("cliente-y".to_string());

    let dirs = [data_dir(&unnamed), data_dir(&named), data_dir(&other)];
    for (i, a) in dirs.iter().enumerate() {
        for b in dirs.iter().skip(i + 1) {
            assert_ne!(a, b, "two instances would write their attachments together");
            assert!(
                !a.starts_with(b) && !b.starts_with(a),
                "and neither may contain the other: {} vs {}",
                a.display(),
                b.display()
            );
        }
    }
}

/// and the unnamed instance's filestore stays **exactly** where it has always
/// been.
///
/// this one is a freeze, in the spirit of `tests/apt_packages.rs`: moving it
/// would be tidier — inside the instance's own install dir, like the named ones
/// — and it would silently orphan every existing customer's attachments, since
/// Odoo would start against an empty directory and report no error at all. the
/// collision this path could have caused no longer exists, so there is nothing
/// to buy with that risk.
#[test]
fn the_unnamed_instances_filestore_does_not_move() {
    let c = ctx(PathBuf::from("/opt/odoo/odoo18"));
    assert_eq!(
        data_dir(&c),
        PathBuf::from("/opt/odoo/.local/share/Odoo"),
        "an existing installation's attachments live here: this path is not ours to improve"
    );
}
