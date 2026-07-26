//! Test di [`InstallPythonRequirements`] (Fase 6): undo no-op + workaround gevent.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::step::Step;
use odoo_installer::steps::install_python_requirements::{
    extract_gevent_spec, filter_out_gevent, InstallPythonRequirements,
};

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

const REQUIREMENTS: &str = "gevent==21.12.0 ; sys_platform != 'win32'\npytz\nBabel==2.9.1\n";

/// Estrae gli argomenti delle sole RunAsUser (le install pip), in ordine.
fn pip_calls(ops: &[Op]) -> Vec<Vec<String>> {
    ops.iter()
        .filter_map(|o| match o {
            Op::RunAsUser { program, args, .. } if program.ends_with("pip") => Some(args.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn undo_is_noop_pip_removal_belongs_to_venv() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    let after_run = ops_of(&log).len();

    step.undo(&c).expect("undo");
    let after_undo = ops_of(&log).len();

    // L'undo non esegue NULLA: nessuna disinstallazione, nessun rm.
    assert_eq!(after_run, after_undo, "9c.undo deve essere no-op");
    assert!(!ops_of(&log).iter().any(|o| matches!(o, Op::RemoveDirAll(_))));
}

#[test]
fn gevent_cython_workaround_sequence() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert_eq!(calls.len(), 4, "quattro passaggi pip attesi");

    // 1) upgrade pip wheel
    assert!(calls[0].contains(&"--upgrade".to_string()) && calls[0].contains(&"pip".to_string()));
    // 2) Cython<3
    assert!(calls[1].contains(&"Cython<3".to_string()));
    // 3) gevent con --no-build-isolation e la spec estratta
    assert!(calls[2].contains(&"--no-build-isolation".to_string()));
    assert!(calls[2].contains(&"gevent==21.12.0".to_string()));
    // 4) resto dei requirements, --prefer-binary, senza gevent
    assert!(calls[3].contains(&"--prefer-binary".to_string()));
    assert!(calls[3].contains(&"--requirement".to_string()));
    assert!(!calls[3].iter().any(|a| a.contains("gevent")));
}

#[test]
fn missing_requirements_is_error() {
    let cfg = MockConfig {
        requirements_content: None, // requirements.txt assente
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut step = InstallPythonRequirements::with_parts(Box::new(mock), dir.path().to_path_buf());
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "requirements.txt mancante → errore");
    assert!(pip_calls(&ops_of(&log)).is_empty(), "nessuna install se manca requirements");
}

#[test]
fn gevent_extraction_and_filtering() {
    assert_eq!(extract_gevent_spec(REQUIREMENTS), "gevent==21.12.0");
    // Senza gevent → default.
    assert_eq!(extract_gevent_spec("pytz\nBabel\n"), "gevent");

    let filtered = filter_out_gevent(REQUIREMENTS);
    assert!(!filtered.to_lowercase().contains("gevent"));
    assert!(filtered.contains("pytz"));
    assert!(filtered.contains("Babel==2.9.1"));
}
