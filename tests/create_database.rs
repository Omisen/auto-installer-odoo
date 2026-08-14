//! [`CreateDatabase`]: the anti-drop protection — a pre-existing database is
//! NEVER dropped.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::create_database::CreateDatabase;

fn ctx() -> Context {
    Context {
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        dry_run: false,
        ..Default::default()
    }
}

fn prestate(step: &CreateDatabase) -> PreState {
    serde_json::from_value(step.snapshot_value()).expect("prestate")
}

#[test]
fn preexisting_database_is_never_dropped() {
    // THE critical test: an existing database makes the undo a strict no-op.
    let cfg = MockConfig {
        db_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDatabase::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(prestate(&step), PreState::Preexisting);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        !ops.iter().any(|o| matches!(o, Op::DropDb(_))),
        "un database preesistente non deve MAI essere droppato: {ops:?}"
    );
    // it was not even created: it was already there.
    assert!(!ops.iter().any(|o| matches!(o, Op::CreateDb { .. })));
}

#[test]
fn absent_database_is_created_and_dropped() {
    let cfg = MockConfig {
        db_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDatabase::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(prestate(&step), PreState::Untracked);

    step.run(&c).expect("run");
    assert_eq!(prestate(&step), PreState::CreatedByUs);

    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.contains(&Op::CreateDb {
        owner: "odoo".to_string(),
        db: "odoo".to_string(),
    }));
    assert!(ops.contains(&Op::DropDb("odoo".to_string())));
}

#[test]
fn preexisting_database_never_dropped_even_after_run() {
    // no path leads to a drop on a pre-existing database, not even calling the
    // undo repeatedly.
    let cfg = MockConfig {
        db_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDatabase::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo 1");
    step.undo(&c).expect("undo 2");

    assert!(!ops_of(&log).iter().any(|o| matches!(o, Op::DropDb(_))));
}
