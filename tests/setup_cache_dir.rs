//! Test di [`SetupCacheDir`] (R6-hotfix-3, A-R5-3 seconda metà): `.cache` nella
//! home dell'utente `odoo` diventa un artefatto posseduto, quindi annullabile.
//!
//! La domanda che questo step cambia non è "chi ha scritto nella cache" — sono
//! programmi di terzi, e cambiano fra versioni — ma "di chi **è** la directory".
//! Il numero di produttori diventa irrilevante: se l'abbiamo creata noi il
//! rollback la rimuove, se c'era già non si tocca.

mod common;

use std::path::{Path, PathBuf};

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::setup_cache_dir::{CacheDirSnapshot, SetupCacheDir};

fn ctx(home: &Path) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        odoo_home: home.to_path_buf(),
        install_dir: home.join("odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

fn mock() -> (MockSystemOps, common::OpLog) {
    MockSystemOps::new(MockConfig {
        real_fs: true, // `path_exists` deve guardare la tempdir vera
        ..Default::default()
    })
}

fn snapshot_of(step: &SetupCacheDir) -> CacheDirSnapshot {
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
fn a_cache_we_created_is_removed_by_the_rollback() {
    // Il caso di campo: dopo un'installazione completa `/opt/odoo/.cache` restava
    // lì perché nessuno step la reclamava. Ora è nostra, e sparisce.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Untracked);
    assert_eq!(snap.created_root, Some(home.path().join(".cache")));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::MkdirAsUser {
            user: "odoo".to_string(),
            path: home.path().join(".cache"),
        }),
        "la cache va creata come utente odoo: chi ci scriverà è lui, non root. {:?}",
        ops_of(&log)
    );
    assert_eq!(snapshot_of(&step).prestate, PreState::CreatedByUs);

    step.undo(&c).expect("undo");
    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".cache")]
    );
}

#[test]
fn a_preexisting_cache_belongs_to_the_client_and_is_never_touched() {
    // `/opt/odoo` può essere una home preesistente con dentro roba del cliente.
    // Se `.cache` c'era già, non la creiamo e soprattutto non la rimuoviamo.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join(".cache")).expect("mkdir .cache");

    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.prestate, PreState::Preexisting);
    assert_eq!(snap.created_root, None);

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).is_empty(),
        "cache preesistente: né creata né rimossa. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_verdict_crosses_the_disk_boundary() {
    // Il rollback da disco ricostruisce lo step da zero: senza reidratare il
    // PreState l'undo sarebbe inerte e la cache resterebbe — che è esattamente il
    // residuo da cui parte questo step.
    let home = tempfile::tempdir().expect("tempdir");

    let (ops, _log) = mock();
    let mut live = SetupCacheDir::with_ops(Box::new(ops));
    let c = ctx(home.path());
    live.snapshot(&c).expect("snapshot");
    live.run(&c).expect("run");
    let persisted = live.snapshot_value();

    let (ops, log) = mock();
    let mut from_disk = SetupCacheDir::with_ops(Box::new(ops));
    from_disk.rehydrate(&persisted).expect("rehydrate");
    from_disk.undo(&c).expect("undo");

    assert_eq!(
        removed_paths(&ops_of(&log)),
        vec![home.path().join(".cache")],
        "lo stato persistito deve bastare a sapere che la cache è nostra"
    );
}

#[test]
fn a_snapshot_pointing_outside_the_home_removes_nothing() {
    // Stessa rete di `setup-data-dir`: è un rm -rf guidato da un path che arriva
    // dal disco, e uno stato corrotto non deve diventare un disastro altrove.
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));

    for hostile in ["/etc", "/", "/opt/odoo-altrove"] {
        let snapshot = serde_json::json!({
            "prestate": "CreatedByUs",
            "created_root": hostile,
        });
        step.rehydrate(&snapshot).expect("rehydrate");
        step.undo(&ctx(home.path())).expect("undo best-effort");
    }
    // E il caso limite: created_root == la home stessa.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path(),
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&ctx(home.path())).expect("undo best-effort");

    assert!(
        removed_paths(&ops_of(&log)).is_empty(),
        "nessuna rimozione fuori dal perimetro della home: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_mutates_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let (ops, log) = mock();
    let mut step = SetupCacheDir::with_ops(Box::new(ops));
    let mut c = ctx(home.path());
    c.dry_run = true;

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    // Il dry-run non passa a CreatedByUs: forziamo lo stato per provare anche il
    // ramo di rimozione.
    let snapshot = serde_json::json!({
        "prestate": "CreatedByUs",
        "created_root": home.path().join(".cache"),
    });
    step.rehydrate(&snapshot).expect("rehydrate");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "dry-run: nessuna operazione. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_cache_is_undone_after_everything_that_could_write_into_it() {
    // L'ordine non è un dettaglio: gli undo girano al contrario, quindi
    // `setup-cache-dir` deve stare PRESTO nella sequenza per essere rimossa TARDI
    // nel rollback — dopo che il servizio è fermo, il venv è sparito e nessuno
    // può più ricreare la cache appena cancellata.
    let make_ops = odoo_installer::system_ops::backend_factory(Default::default())
        .expect("la famiglia Debian ha un backend");
    let names = odoo_installer::steps::canonical_step_names(&make_ops);
    let pos = |name: &str| {
        names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("'{name}' deve essere nella sequenza canonica"))
    };

    let cache = pos("setup-cache-dir");
    for later in [
        "install-python-requirements", // pip scrive cache
        "initialize-odoo-database",    // odoo-bin carica font, ecc.
        "setup-systemd",               // il servizio gira e scrive
        "create-virtualenv",
    ] {
        assert!(
            cache < pos(later),
            "setup-cache-dir deve precedere '{later}' nella sequenza, così il suo undo \
             gira dopo quello di '{later}'"
        );
    }
}
