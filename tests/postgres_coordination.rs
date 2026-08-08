//! Coordinamento ruolo/DB (Fase 5): al rollback l'undo del database precede
//! l'undo del ruolo (ordine inverso), così il DB sparisce prima del ruolo che
//! lo possiede.

mod common;

use std::sync::{Arc, Mutex};

use common::{ops_of, MockConfig, MockSystemOps, Op, OpLog};
use invok::context::Context;
use invok::engine::Installer;
use invok::step::Step;
use invok::steps::create_database::CreateDatabase;
use invok::steps::create_db_role::CreateDbRole;
use invok::steps::noop::NoopStep;

#[test]
fn rollback_drops_database_before_role() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Log condiviso fra i due step, per osservarne l'ordine reciproco.
    let log: OpLog = Arc::new(Mutex::new(Vec::new()));

    let role_ops = MockSystemOps::with_log(
        MockConfig {
            role_exists: false,
            ..Default::default()
        },
        Arc::clone(&log),
    );
    let db_ops = MockSystemOps::with_log(
        MockConfig {
            db_exists: false,
            ..Default::default()
        },
        Arc::clone(&log),
    );

    // Ordine di produzione: ruolo → database → (step che fallisce).
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(CreateDbRole::with_ops(Box::new(role_ops))),
        Box::new(CreateDatabase::with_ops(Box::new(db_ops))),
        Box::new(NoopStep::new("boom").fail_on_run()),
    ];

    let ctx = Context {
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"));

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);
    assert!(
        result.is_err(),
        "lo step finale fallisce e innesca il rollback"
    );

    let ops = ops_of(&log);
    let drop_db = ops.iter().position(|o| matches!(o, Op::DropDb(_)));
    let drop_role = ops.iter().position(|o| matches!(o, Op::PgDropRole(_)));

    assert!(
        drop_db.is_some(),
        "il DB creato da noi deve essere droppato"
    );
    assert!(
        drop_role.is_some(),
        "il ruolo creato da noi deve essere droppato"
    );
    assert!(
        drop_db < drop_role,
        "l'undo del database deve precedere l'undo del ruolo (ordine inverso), ops: {ops:?}"
    );
}
