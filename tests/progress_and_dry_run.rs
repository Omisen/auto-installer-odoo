//! the progress reporter and the `--dry-run` plan.

mod common;

use common::{events_of, ops_of, MockConfig, MockSystemOps, RecordingReporter};
use invok::context::Context;
use invok::engine::{dry_run_plan, Installer};
use invok::progress::NoopReporter;
use invok::step::Step;
use invok::steps::create_odoo_user::CreateOdooUser;
use invok::steps::noop::NoopStep;

fn ctx_state(dir: &tempfile::TempDir, dry_run: bool) -> Context {
    Context {
        dry_run,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"))
}

#[test]
fn reporter_notified_on_success() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (reporter, events) = RecordingReporter::new();
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];
    let mut installer = Installer::new();

    installer
        .execute_with_reporter(&mut steps, &ctx_state(&dir, false), &reporter)
        .expect("ok");

    let ev = events_of(&events);
    assert_eq!(
        ev,
        vec!["start:alpha", "done:alpha", "start:beta", "done:beta"]
    );
}

#[test]
fn reporter_notified_on_failure_and_rollback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (reporter, events) = RecordingReporter::new();
    // the first two complete, the third fails, and both are rolled back.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
        Box::new(NoopStep::new("gamma").fail_on_run()),
    ];
    let mut installer = Installer::new();

    let result = installer.execute_with_reporter(&mut steps, &ctx_state(&dir, false), &reporter);
    assert!(result.is_err());

    let ev = events_of(&events);
    assert!(ev.contains(&"failed:gamma".to_string()));
    assert!(ev.contains(&"rollback".to_string()));
    // undone in reverse order.
    let ub = ev.iter().position(|e| e == "undo:beta").expect("undo beta");
    let ua = ev
        .iter()
        .position(|e| e == "undo:alpha")
        .expect("undo alpha");
    assert!(ub < ua, "undo in ordine inverso");
}

#[test]
fn noop_reporter_is_usable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(NoopStep::new("solo"))];
    let mut installer = Installer::new();
    installer
        .execute_with_reporter(&mut steps, &ctx_state(&dir, false), &NoopReporter)
        .expect("ok");
}

#[test]
fn dry_run_plan_does_not_mutate() {
    // the plan calls snapshot and a dry `run`: no mutating operation may reach
    // `SystemOps`.
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let (reporter, events) = RecordingReporter::new();
    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(CreateOdooUser::with_ops(Box::new(mock)))];

    let ctx = Context {
        odoo_user: "odoo".to_string(),
        odoo_home: std::path::PathBuf::from("/opt/odoo"),
        dry_run: true,
        ..Default::default()
    };

    dry_run_plan(&mut steps, &ctx, &reporter);

    // no mutation at all: the operations log is empty.
    assert!(
        ops_of(&log).is_empty(),
        "dry-run non deve mutare, trovato: {:?}",
        ops_of(&log)
    );
    // the plan lists the step.
    assert_eq!(
        events_of(&events),
        vec!["start:create-odoo-user", "done:create-odoo-user"]
    );
}

#[test]
fn dry_run_execute_persists_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let ctx = Context {
        dry_run: true,
        ..Default::default()
    }
    .with_state_path(state_path.clone());

    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(NoopStep::new("solo"))];
    let mut installer = Installer::new();
    installer
        .execute_with_reporter(&mut steps, &ctx, &NoopReporter)
        .expect("ok");

    assert!(
        !state_path.exists(),
        "in dry-run lo stato non deve essere persistito"
    );
}
