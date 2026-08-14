//! [`SetupLogDir`]: disabled means no-op; enabled creates and removes.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::setup_log_dir::SetupLogDir;

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

    assert!(
        ops_of(&log).is_empty(),
        "logfile disabilitato → nessuna azione"
    );
}

#[test]
fn created_by_us_creates_and_removes() {
    // missing log directory: created, then removed while empty.
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
    assert!(ops.iter().any(
        |op| matches!(op, Op::ChownNamed { owner, group, .. } if owner == "odoo" && group == "odoo")
    ));
    assert!(ops
        .iter()
        .any(|op| matches!(op, Op::Chmod { mode, .. } if *mode == 0o750)));
    assert!(
        ops.contains(&Op::Rmdir(dir)),
        "the undo must remove the directory we created, when empty"
    );
}

#[test]
fn preexisting_dir_is_not_touched() {
    // already there: nothing created, and the undo leaves it.
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

    assert!(
        ops_of(&log).is_empty(),
        "a Preexisting log dir is not touched"
    );
}

#[test]
fn undo_does_not_remove_non_empty_dir() {
    // ours but not empty, because logs were written: the undo leaves it.
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
        "the undo must not remove a non-empty log dir"
    );
}
