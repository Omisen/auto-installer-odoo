//! Test di [`CreateDbRole`] (Fase 5): creazione ruolo, escape, password non loggata.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::secret::Secret;
use odoo_installer::step::Step;
use odoo_installer::steps::create_db_role::CreateDbRole;
use odoo_installer::system_ops::escape_sql_literal;

fn ctx(db_user: &str, password: Option<&str>) -> Context {
    Context {
        db_user: db_user.to_string(),
        db_password: password.map(Secret::new).unwrap_or_default(),
        dry_run: false,
        ..Default::default()
    }
}

#[test]
fn absent_role_is_created_and_dropped() {
    let cfg = MockConfig {
        role_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDbRole::with_ops(Box::new(mock));
    let c = ctx("odoo", None); // peer auth (nessuna password)

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.contains(&Op::PgCreateRole {
        role: "odoo".to_string(),
        has_password: false,
    }));
    assert!(ops.contains(&Op::PgDropRole("odoo".to_string())));
}

#[test]
fn role_with_password_records_only_presence_not_value() {
    let cfg = MockConfig {
        role_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDbRole::with_ops(Box::new(mock));
    // Password con apice singolo (verifica anche che non rompa nulla a valle).
    let c = ctx("odoo", Some("p'wn"));

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    // L'op registra solo has_password: il valore non è mai catturato/loggato.
    assert!(ops.contains(&Op::PgCreateRole {
        role: "odoo".to_string(),
        has_password: true,
    }));
}

#[test]
fn preexisting_role_is_never_touched() {
    let cfg = MockConfig {
        role_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateDbRole::with_ops(Box::new(mock));
    let c = ctx("odoo", None);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(ops_of(&log).is_empty(), "un ruolo preesistente non va né creato né droppato");
}

#[test]
fn sql_literal_escaping_doubles_single_quotes() {
    assert_eq!(escape_sql_literal("p'wn"), "p''wn");
    assert_eq!(escape_sql_literal("a'b'c"), "a''b''c");
    assert_eq!(escape_sql_literal("nessun apice"), "nessun apice");
}
