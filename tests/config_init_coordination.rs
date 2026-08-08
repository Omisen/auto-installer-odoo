//! Coordinamento CreateDatabase → InitializeOdooDatabase (Fase 5 → 7): il
//! PreState del DB si propaga via `ctx.db_created_by_us` attraverso il motore.

mod common;

use std::sync::{Arc, Mutex};

use common::{ops_of, MockConfig, MockSystemOps, Op, OpLog};
use invok::context::Context;
use invok::engine::Installer;
use invok::step::Step;
use invok::steps::create_database::CreateDatabase;
use invok::steps::initialize_odoo_database::InitializeOdooDatabase;

fn run_chain(db_exists: bool) -> (bool, Vec<Op>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let log: OpLog = Arc::new(Mutex::new(Vec::new()));

    let db_ops = MockSystemOps::with_log(
        MockConfig {
            db_exists,
            ..Default::default()
        },
        Arc::clone(&log),
    );
    let init_ops = MockSystemOps::with_log(
        MockConfig {
            db_initialized: false,
            ..Default::default()
        },
        Arc::clone(&log),
    );

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(CreateDatabase::with_ops(Box::new(db_ops))),
        Box::new(InitializeOdooDatabase::with_ops(Box::new(init_ops))),
    ];

    let ctx = Context {
        db_name: "odoo".to_string(),
        db_user: "odoo".to_string(),
        odoo_user: "odoo".to_string(),
        odoo_version_short: "18".to_string(),
        install_dir: std::path::PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"));

    let mut installer = Installer::new();
    let ok = installer.execute(&mut steps, &ctx).is_ok();
    (ok, ops_of(&log))
}

#[test]
fn our_db_allows_init() {
    // DB assente → creato da noi → CreateDatabase pubblica db_created_by_us=true
    // → InitializeOdooDatabase procede.
    let (ok, ops) = run_chain(/* db_exists */ false);
    assert!(ok, "la catena deve completare");
    assert!(ops.iter().any(|o| matches!(o, Op::CreateDb { .. })));
    assert!(
        ops.iter().any(|o| matches!(o, Op::OdooInitBase { .. })),
        "su DB nostro l'init procede"
    );
}

#[test]
fn preexisting_db_blocks_init_through_engine() {
    // DB preesistente → db_created_by_us=false → hard-stop di init → l'intera
    // catena fallisce e l'init non parte mai.
    let (ok, ops) = run_chain(/* db_exists */ true);
    assert!(!ok, "il DB preesistente deve bloccare la catena");
    assert!(
        !ops.iter().any(|o| matches!(o, Op::OdooInitBase { .. })),
        "init mai eseguito su DB preesistente, nemmeno attraverso il motore"
    );
}
