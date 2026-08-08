//! Test di [`InitializeOdooDatabase`] (Fase 7): l'hard-stop su DB preesistente.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::initialize_odoo_database::InitializeOdooDatabase;

fn ctx(db_created_by_us: bool) -> Context {
    Context {
        db_name: "odoo".to_string(),
        odoo_user: "odoo".to_string(),
        odoo_version_short: "18".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        db_created_by_us: Arc::new(AtomicBool::new(db_created_by_us)),
        ..Default::default()
    }
}

fn prestate(step: &InitializeOdooDatabase) -> PreState {
    serde_json::from_value(step.snapshot_value()).expect("prestate")
}

#[test]
fn hard_stop_on_preexisting_db_never_inits() {
    // IL test critico: DB preesistente (non nostro), schema assente → ERRORE,
    // e `odoo-bin -i base` NON viene mai eseguito.
    let cfg = MockConfig {
        db_initialized: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InitializeOdooDatabase::with_ops(Box::new(mock));
    let c = ctx(/* db_created_by_us */ false);

    let result = step.snapshot(&c);
    assert!(
        result.is_err(),
        "init su DB preesistente deve essere rifiutato (hard-stop)"
    );
    // A3.3: il messaggio deve suggerire l'ipotesi "residuo di una run fallita",
    // altrimenti l'utente resta bloccato senza capire perché.
    let msg = result.expect_err("errore").to_string();
    assert!(
        msg.contains("residuo"),
        "il messaggio deve offrire la diagnosi 'residuo di run precedente': {msg}"
    );
    assert!(
        msg.contains("dropdb odoo"),
        "il messaggio deve indicare l'azione concreta: {msg}"
    );
    assert!(
        !ops_of(&log)
            .iter()
            .any(|o| matches!(o, Op::OdooInitBase { .. })),
        "l'init NON deve mai essere invocato su un DB preesistente"
    );
}

#[test]
fn created_by_us_runs_init_and_undo_is_noop() {
    let cfg = MockConfig {
        db_initialized: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InitializeOdooDatabase::with_ops(Box::new(mock));
    let c = ctx(/* db_created_by_us */ true);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(prestate(&step), PreState::Untracked);

    step.run(&c).expect("run");
    let after_run = ops_of(&log).len();

    step.undo(&c).expect("undo");
    assert_eq!(
        ops_of(&log).len(),
        after_run,
        "undo è no-op (pulizia coperta dal dropdb di Fase 5)"
    );

    assert!(ops_of(&log)
        .iter()
        .any(|o| matches!(o, Op::OdooInitBase { db, conf }
        if db == "odoo" && conf == Path::new("/opt/odoo/odoo18/odoo18.conf"))));
}

#[test]
fn already_initialized_schema_is_noop_even_if_preexisting_db() {
    // Schema già presente → no-op, e la presenza dello schema precede l'hard-stop
    // (nessun errore anche se il DB non è nostro).
    let cfg = MockConfig {
        db_initialized: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InitializeOdooDatabase::with_ops(Box::new(mock));
    let c = ctx(/* db_created_by_us */ false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(prestate(&step), PreState::Preexisting);
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(!ops_of(&log)
        .iter()
        .any(|o| matches!(o, Op::OdooInitBase { .. })));
}
