//! Test di [`CreateOdooUser`] (Fase 3): logica di decisione via mock, senza root.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::create_odoo_user::{CreateOdooUser, CreateUserSnapshot};
use odoo_installer::system_ops::OwnerId;

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_home: PathBuf::from("/opt/odoo"),
        dry_run: false,
        ..Default::default()
    }
}

fn persisted(step: &CreateOdooUser) -> CreateUserSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

#[test]
fn created_by_us_runs_useradd_and_undo_userdel_without_r() {
    // Utente assente; home inesistente (per isolare la parte useradd/userdel).
    let cfg = MockConfig {
        user_exists: false,
        path_exists: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert_eq!(persisted(&step).user_prestate, PreState::CreatedByUs);

    step.undo(&c).expect("undo");

    let ops = ops_of(&log);

    // run: useradd con gli argomenti attesi.
    let created = ops.iter().find_map(|op| match op {
        Op::CreateUser(spec) => Some(spec),
        _ => None,
    });
    let spec = created.expect("useradd deve essere eseguito");
    assert_eq!(spec.name, "odoo");
    assert_eq!(spec.home, PathBuf::from("/opt/odoo"));
    assert!(spec.system && spec.create_home && spec.user_group);
    assert_eq!(spec.shell, "/bin/false");

    // run: chown esplicito odoo:odoo + chmod 0750 sulla home.
    assert!(ops.iter().any(|op| matches!(op, Op::ChownNamed { owner, group, .. } if owner == "odoo" && group == "odoo")));
    assert!(ops.iter().any(|op| matches!(op, Op::Chmod { mode, .. } if *mode == 0o750)));

    // undo: userdel + groupdel, MAI con la home (nessun path/`-r`).
    assert!(ops.contains(&Op::DeleteUser("odoo".to_string())));
    assert!(ops.contains(&Op::DeleteGroup("odoo".to_string())));
    // Struttura del confine: DeleteUser porta solo il nome utente, nessun path.
    // (la home la rimuove PrepareOptRoot.undo).
}

#[test]
fn preexisting_user_is_never_touched() {
    // Utente già presente: niente useradd in run, niente userdel in undo.
    let cfg = MockConfig {
        user_exists: true,
        path_exists: true,
        owner: OwnerId { uid: 0, gid: 0 },
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step).user_prestate, PreState::Preexisting);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // Nessuna mutazione: un utente preesistente non viene mai toccato.
    assert!(ops.is_empty(), "un utente Preexisting non deve subire alcuna azione, trovato: {ops:?}");
}

#[test]
fn undo_restores_original_owner_when_home_was_preexisting() {
    // Home preesistente owned uid/gid 1000: dopo il nostro chown a odoo, l'undo
    // deve ripristinare l'owner originale (non lasciarla a un utente cancellato).
    let original = OwnerId {
        uid: 1000,
        gid: 1000,
    };
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        owner: original,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert_eq!(persisted(&step).home_original_owner, Some(original));

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::ChownNumeric {
            path: PathBuf::from("/opt/odoo"),
            id: original,
        }),
        "undo deve ripristinare l'owner originale della home, trovato: {ops:?}"
    );
    // E comunque userdel senza -r.
    assert!(ops.contains(&Op::DeleteUser("odoo".to_string())));
}

#[test]
fn dry_run_creates_nothing() {
    let cfg = MockConfig {
        user_exists: false,
        path_exists: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = CreateOdooUser::with_ops(Box::new(mock));
    let mut c = ctx();
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // In dry-run lo stato resta Untracked → undo NO-OP.
    assert_eq!(persisted(&step).user_prestate, PreState::Untracked);
    step.undo(&c).expect("undo");

    assert!(ops_of(&log).is_empty(), "dry-run non deve eseguire alcuna operazione");
}
