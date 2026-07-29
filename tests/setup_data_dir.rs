//! Test di [`SetupDataDir`] (R6, A-R5-3): il filestore di Odoo diventa un
//! artefatto registrato, e quindi annullabile — ma solo con la stessa protezione
//! che difende il database del cliente.
//!
//! I due assi da tenere separati sono la **proprietà della directory**
//! (`PreState`: l'abbiamo creata noi?) e la **proprietà dei dati** (il database
//! era nostro?). Servono entrambi: la prima da sola porterebbe a cancellare gli
//! allegati di un DB preesistente, la seconda da sola a cancellare una directory
//! che non abbiamo creato.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::generate_config;
use odoo_installer::steps::setup_data_dir::{DataDirSnapshot, SetupDataDir};

/// Context su una home reale (tempdir), così `path_exists` risponde davvero:
/// i livelli `.local` / `.local/share` vanno distinti uno per uno.
fn ctx(home: &Path, db_is_ours: bool) -> Context {
    let c = Context {
        odoo_user: "odoo".to_string(),
        odoo_home: home.to_path_buf(),
        install_dir: home.join("odoo18"),
        db_name: "odoo".to_string(),
        dry_run: false,
        ..Default::default()
    };
    c.db_created_by_us.store(db_is_ours, Ordering::SeqCst);
    c
}

fn mock() -> (MockSystemOps, common::OpLog) {
    MockSystemOps::new(MockConfig {
        real_fs: true, // path_exists guarda la tempdir vera
        ..Default::default()
    })
}

fn snapshot_of(step: &SetupDataDir) -> DataDirSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

fn removed_paths(ops: &[Op]) -> Vec<PathBuf> {
    ops.iter()
        .filter_map(|o| match o {
            Op::RemoveDirAll(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn creates_the_filestore_and_removes_the_topmost_level_it_created() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);
    let data_dir = generate_config::data_dir(&c);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Untracked);
    assert_eq!(
        snap.created_root,
        Some(home.path().join(".local")),
        "niente esisteva: la radice creata da noi è `.local`"
    );
    assert!(snap.db_was_ours, "il DB non era preesistente");

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::MkdirAsUser {
            user: "odoo".to_string(),
            path: data_dir.clone(),
        }),
        "il filestore va creato come utente odoo: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        snapshot_of(&step).prestate,
        PreState::CreatedByUs,
        "dopo il run la directory è nostra"
    );

    step.undo(&c).expect("undo");
    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".local")],
        "l'undo rimuove la radice creata da noi, in un colpo"
    );
}

#[test]
fn a_preexisting_dot_local_is_not_touched() {
    // Il cliente ha già `/opt/odoo/.local` (ci tiene altro). Noi creiamo solo
    // `share/Odoo` sotto, e l'undo deve fermarsi a `share`: `.local` non è nostra.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join(".local")).expect("mkdir .local");

    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        snapshot_of(&step).created_root,
        Some(home.path().join(".local").join("share")),
        "il primo livello mancante è `share`, non `.local`"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let removed = removed_paths(&ops_of(&log));
    assert_eq!(removed, vec![home.path().join(".local").join("share")]);
    assert!(
        !removed.contains(&home.path().join(".local")),
        "una directory preesistente del cliente non va mai rimossa: {removed:?}"
    );
}

#[test]
fn a_preexisting_filestore_is_left_alone_entirely() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), true);
    std::fs::create_dir_all(generate_config::data_dir(&c)).expect("mkdir data_dir");

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Preexisting);
    assert_eq!(snap.created_root, None);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).is_empty(),
        "filestore preesistente: né creazione né rimozione. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_filestore_of_a_preexisting_database_is_never_removed() {
    // PROTEZIONE CRITICA. La directory l'abbiamo creata noi (`CreatedByUs`), ma
    // il database era del cliente: dentro ci sono i suoi allegati. `CreateDatabase`
    // non droppa quel DB, e qui non si cancella il suo filestore.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let c = ctx(home.path(), false); // DB preesistente

    step.snapshot(&c).expect("snapshot");
    assert!(!snapshot_of(&step).db_was_ours);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "il filestore di un DB preesistente non va rimosso: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_database_verdict_crosses_the_disk_boundary() {
    // Il caso reale del comando `rollback`: il Context è ricostruito dalla config
    // persistita, dove `db_created_by_us` vale `false` di default. Se l'undo
    // leggesse quel flag invece del proprio snapshot, il filestore di
    // un'installazione nostra non verrebbe mai rimosso (residuo) — e con il flag
    // invertito succederebbe il peggio: quello di un cliente rimosso per errore.
    let home = tempfile::tempdir().expect("tempdir");

    // Installazione: DB nostro, filestore creato da noi.
    let (ops, _log) = mock();
    let mut live = SetupDataDir::with_ops(Box::new(ops));
    let install_ctx = ctx(home.path(), true);
    live.snapshot(&install_ctx).expect("snapshot");
    live.run(&install_ctx).expect("run");
    let persisted = live.snapshot_value();

    // Rollback da disco: Context "vergine" (flag a false), step reidratato.
    let (ops, log) = mock();
    let mut from_disk = SetupDataDir::with_ops(Box::new(ops));
    from_disk.rehydrate(&persisted).expect("rehydrate");
    let rollback_ctx = ctx(home.path(), false);
    from_disk.undo(&rollback_ctx).expect("undo");

    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".local")],
        "il verdetto sul DB va riletto dallo snapshot, non dedotto dal Context"
    );
}

#[test]
fn a_snapshot_pointing_outside_the_perimeter_removes_nothing() {
    // Fail-closed su uno stato corrotto (o scritto da un'altra installazione): un
    // `created_root` fuori da `odoo_home` non deve diventare un rm -rf altrove.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));

    for hostile in ["/etc", "/", "/opt/odoo-altrove"] {
        let snapshot = serde_json::json!({
            "prestate": "CreatedByUs",
            "created_root": hostile,
            "db_was_ours": true,
        });
        step.rehydrate(&snapshot).expect("rehydrate");
        step.undo(&ctx(home.path(), true))
            .expect("undo best-effort");
    }
    // E anche il caso limite: created_root == odoo_home.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path(),
        "db_was_ours": true,
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&ctx(home.path(), true))
        .expect("undo best-effort");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "nessuna rimozione fuori dal perimetro: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_mutates_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupDataDir::with_ops(Box::new(ops));
    let mut c = ctx(home.path(), true);
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // Il dry-run non passa a CreatedByUs, quindi l'undo è inerte per costruzione:
    // forziamo lo stato "creato da noi" per provare anche il ramo di rimozione.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path().join(".local"),
        "db_was_ours": true,
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "dry-run: nessuna operazione sul sistema. Trovato: {:?}",
        ops_of(&log)
    );
}
