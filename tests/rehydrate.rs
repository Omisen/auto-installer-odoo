//! Simmetria `snapshot_value` ⇄ `rehydrate` (R4): la proprietà che rende
//! affidabile il rollback da disco.
//!
//! Il rollback da stato persistito ricostruisce ogni step e gli rimette dentro
//! lo snapshot dell'epoca. Se quella reidratazione perde anche un solo campo,
//! l'`undo` prende la decisione sbagliata — e la decisione sbagliata più grave è
//! droppare un database che lo snapshot marcava `Preexisting`. Perciò qui non si
//! prova "il rollback funziona": si prova che **reidratato ≡ originale**, step
//! per step e a livello di comportamento.
//!
//! Tre livelli di verifica:
//! 1. **identità del JSON** — `snapshot_value` dopo `rehydrate` è identico a
//!    prima, per ogni step della catena;
//! 2. **equivalenza dell'undo** — la stessa catena annullata dagli step vivi e
//!    dagli step reidratati porta il sistema allo **stesso** stato finale;
//! 3. **fixture di campo** — lo state file realmente osservato su VM dopo un
//!    Ctrl-C viene reidratato e produce gli undo attesi.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use odoo_installer::context::Context;
use odoo_installer::secret::Secret;
use odoo_installer::state::{InstallState, PreState};
use odoo_installer::step::Step;
use odoo_installer::steps;
use odoo_installer::system_ops::SystemOps;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";

/// La catena reale ricostruibile dalla factory, con `SystemOps` mockabili.
///
/// Esclusi `prepare-opt-root` (usa `std::fs` diretto),
/// `install-python-requirements` (scrive un temp reale) e
/// `install-wkhtmltopdf` (la factory gli dà il downloader reale: il suo `run`
/// scaricherebbe davvero). I loro cicli hanno test dedicati.
const CHAIN: &[&str] = &[
    "create-odoo-user",
    "setup-log-dir",
    "bootstrap-prerequisites",
    "install-system-dependencies",
    "setup-postgres",
    "create-db-role",
    "create-database",
    "clone-odoo-repo",
    "create-virtualenv",
    "generate-config",
    "setup-data-dir",
    "initialize-odoo-database",
    "setup-systemd",
    "nginx-install",
    "nginx-write-config",
    "nginx-enable-site",
    "nginx-firewall",
    "nginx-reload",
    "write-control-script",
    "patch-bashrc",
];

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn paths(items: &[&str]) -> HashSet<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

fn fresh_state() -> ModelState {
    let mut contents = HashMap::new();
    contents.insert(PathBuf::from(BASHRC), BASHRC_ORIG.to_string());
    ModelState {
        paths: paths(&[HOME, SUDO_HOME, BASHRC]),
        file_contents: contents,
        packages: set(&["coreutils"]),
        sudo_home: Some(SUDO_HOME.to_string()),
        ufw_available: true,
        ufw_active: true,
        ..Default::default()
    }
}

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        db_password: Secret::default(),
        admin_passwd: Secret::new("s3cret"),
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(INSTALL),
        port: 8069,
        with_nginx: true,
        nginx_server_name: "_".to_string(),
        sudo_user: Some("alice".to_string()),
        aggressive_rollback: true,
        ..Default::default()
    }
}

/// Costruisce la catena dalla factory, legando ogni step al modello dato.
fn chain_from_factory(model: &SystemModel, names: &[&str]) -> Vec<Box<dyn Step>> {
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    names
        .iter()
        .map(|name| {
            steps::step_by_name(name, &make_ops)
                .unwrap_or_else(|| panic!("la factory deve conoscere lo step '{name}'"))
        })
        .collect()
}

/// Porta la catena fino in fondo (snapshot + run) e ritorna gli snapshot JSON.
fn run_chain(steps: &mut [Box<dyn Step>], ctx: &Context) -> Vec<serde_json::Value> {
    for step in steps.iter_mut() {
        let name = step.name().to_string();
        step.snapshot(ctx)
            .unwrap_or_else(|e| panic!("snapshot di '{name}' fallito: {e}"));
        step.run(ctx)
            .unwrap_or_else(|e| panic!("run di '{name}' fallito: {e}"));
    }
    steps.iter().map(|s| s.snapshot_value()).collect()
}

// --- 1. Identità del JSON, step per step ------------------------------------

#[test]
fn every_step_rehydrates_to_an_identical_snapshot() {
    // Livello più fine della verifica: per **ogni** step della catena, il JSON
    // prodotto dopo la reidratazione deve essere identico a quello di partenza.
    // Un campo dimenticato in `rehydrate` (o un `Option` che si perde) qui salta
    // fuori subito, e col nome dello step che l'ha perso.
    let model = SystemModel::new(fresh_state());
    let ctx = ctx();
    let mut live = chain_from_factory(&model, CHAIN);
    let snapshots = run_chain(&mut live, &ctx);

    let clean = SystemModel::new(fresh_state());
    for (step, original) in live.iter().zip(snapshots.iter()) {
        let name = step.name();
        let make_ops = || -> Box<dyn SystemOps> { clean.boxed() };
        let mut fresh = steps::step_by_name(name, &make_ops)
            .unwrap_or_else(|| panic!("factory senza '{name}'"));

        fresh
            .rehydrate(original)
            .unwrap_or_else(|e| panic!("rehydrate di '{name}' fallito: {e}"));

        assert_eq!(
            &fresh.snapshot_value(),
            original,
            "'{name}': snapshot_value dopo rehydrate deve essere identico all'originale"
        );
    }
}

#[test]
fn a_corrupt_snapshot_fails_rehydration_instead_of_defaulting() {
    // Fail-closed: uno snapshot illeggibile NON deve produrre uno step con stato
    // di default. `Untracked` di default sarebbe "innocuo" solo per caso — e per
    // `generate-config` o `patch-bashrc` significherebbe non ripristinare il
    // backup del cliente. Meglio un errore, che il rollback segnala come residuo.
    let model = SystemModel::new(fresh_state());
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };

    for name in ["create-database", "setup-postgres", "patch-bashrc"] {
        let mut step = steps::step_by_name(name, &make_ops).expect("factory");
        let bogus = serde_json::json!({ "questo": "non è uno snapshot valido" });
        assert!(
            step.rehydrate(&bogus).is_err(),
            "'{name}': uno snapshot non deserializzabile deve essere un errore"
        );
    }
}

// --- 2. Equivalenza comportamentale dell'undo -------------------------------

#[test]
fn rehydrated_steps_undo_exactly_like_the_live_ones() {
    // La proprietà che conta davvero: non "il JSON è uguale" ma "il sistema
    // finisce nello stesso posto". Due modelli identici, la stessa catena
    // annullata in due modi — dagli step vivi e da step ricostruiti dal solo
    // JSON — devono convergere.
    let ctx = ctx();

    // (a) percorso in-process: gli step che hanno fatto il run annullano.
    let live_model = SystemModel::new(fresh_state());
    let mut live = chain_from_factory(&live_model, CHAIN);
    let snapshots = run_chain(&mut live, &ctx);
    let after_run = live_model.snapshot();
    for step in live.iter().rev() {
        let _ = step.undo(&ctx);
    }
    let undone_in_process = live_model.snapshot();

    // (b) percorso da disco: stesso stato post-run, step nuovi reidratati.
    let disk_model = SystemModel::new(after_run.clone());
    let mut rehydrated = chain_from_factory(&disk_model, CHAIN);
    for (step, snap) in rehydrated.iter_mut().zip(snapshots.iter()) {
        step.rehydrate(snap).expect("rehydrate");
    }
    for step in rehydrated.iter().rev() {
        let _ = step.undo(&ctx);
    }
    let undone_from_disk = disk_model.snapshot();

    assert_eq!(
        undone_from_disk, undone_in_process,
        "l'undo da step reidratati deve portare allo stesso stato dell'undo in-process"
    );
    assert_eq!(
        undone_from_disk,
        fresh_state(),
        "e quello stato è il sistema vergine"
    );
}

#[test]
fn rehydration_without_the_snapshot_would_undo_the_wrong_things() {
    // Contro-prova (validazione per mutazione, in forma di test): se si
    // saltasse `rehydrate` e si annullasse con step "vergini", il rollback
    // sarebbe silenziosamente inerte — ogni PreState resterebbe `Untracked` e
    // ogni undo un NO-OP. È la ragione per cui `rehydrate` esiste, resa
    // esplicita: senza, il sistema NON torna pulito.
    let ctx = ctx();
    let model = SystemModel::new(fresh_state());
    let mut live = chain_from_factory(&model, CHAIN);
    run_chain(&mut live, &ctx);
    let after_run = model.snapshot();

    let disk_model = SystemModel::new(after_run.clone());
    let not_rehydrated = chain_from_factory(&disk_model, CHAIN);
    for step in not_rehydrated.iter().rev() {
        let _ = step.undo(&ctx);
    }

    assert_ne!(
        disk_model.snapshot(),
        fresh_state(),
        "senza rehydrate il rollback non può funzionare: se questo assert cade, \
         gli undo stanno agendo senza consultare il PreState"
    );
    assert!(
        disk_model.snapshot().users.contains("odoo"),
        "senza il PreState reidratato l'utente creato da noi non viene rimosso"
    );
}

// --- 3. Fixture di campo: lo state file osservato su VM ----------------------

/// Lo state file **reale** letto su una VM Multipass dopo un Ctrl-C a metà
/// installazione. È l'input esatto che il comando `rollback` consuma: usarlo
/// come fixture significa provare la reidratazione contro il formato vero, non
/// contro quello che i test producono da soli.
const FIELD_STATE: &str = r#"{
  "completed": [
    { "name": "prepare-opt-root", "snapshot": "Preexisting" },
    {
      "name": "create-odoo-user",
      "snapshot": {
        "home_original_owner": { "uid": 0, "gid": 0 },
        "user_prestate": "CreatedByUs"
      }
    },
    {
      "name": "install-system-dependencies",
      "snapshot": {
        "already_installed": ["git", "curl"],
        "delta": ["python3-pip", "build-essential", "libpq-dev"]
      }
    },
    {
      "name": "setup-postgres",
      "snapshot": {
        "active": "CreatedByUs",
        "enabled": "CreatedByUs",
        "installed": "CreatedByUs"
      }
    }
  ]
}"#;

fn field_state() -> InstallState {
    serde_json::from_str(FIELD_STATE).expect("lo state file di campo deve restare leggibile")
}

#[test]
fn the_real_state_file_still_parses_and_has_no_config() {
    // Retrocompatibilità di formato: un file scritto prima di R4 resta
    // leggibile, e l'assenza di `config` è rilevabile — è ciò su cui il comando
    // `rollback` si ferma invece di indovinare i nomi degli artefatti.
    let state = field_state();
    assert_eq!(state.completed.len(), 4);
    assert_eq!(state.completed[0].name, "prepare-opt-root");
    assert!(
        state.config.is_none(),
        "gli state file pre-R4 non hanno la configurazione: il comando rollback \
         deve poterlo distinguere"
    );
}

/// Reidrata uno step dal fixture di campo e ne esegue l'undo sul modello.
fn undo_from_field_state(model: &SystemModel, step_name: &str, ctx: &Context) {
    let state = field_state();
    let record = state
        .completed
        .iter()
        .find(|r| r.name == step_name)
        .unwrap_or_else(|| panic!("il fixture deve contenere '{step_name}'"));
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let mut step = steps::step_by_name(step_name, &make_ops).expect("factory");
    step.rehydrate(&record.snapshot)
        .unwrap_or_else(|e| panic!("rehydrate di '{step_name}' dal fixture: {e}"));
    step.undo(ctx)
        .unwrap_or_else(|e| panic!("undo di '{step_name}': {e}"));
}

#[test]
fn field_state_create_odoo_user_rehydrates_and_removes_the_user() {
    let mut init = fresh_state();
    init.users.insert("odoo".to_string());
    init.groups.insert("odoo".to_string());
    let model = SystemModel::new(init);

    undo_from_field_state(&model, "create-odoo-user", &ctx());

    let s = model.snapshot();
    assert!(
        !s.users.contains("odoo"),
        "user_prestate=CreatedByUs nel fixture → l'utente va rimosso"
    );
    assert!(!s.groups.contains("odoo"), "e il gruppo dedicato con lui");
    assert!(
        s.paths.contains(&PathBuf::from(HOME)),
        "la home NON è di questo step: userdel senza -r, la rimuove PrepareOptRoot"
    );
}

#[test]
fn field_state_apt_delta_purges_only_the_delta() {
    let mut init = fresh_state();
    // Il sistema come sarebbe a metà installazione: già presenti + delta nostro.
    for pkg in ["git", "curl", "python3-pip", "build-essential", "libpq-dev"] {
        init.packages.insert(pkg.to_string());
    }
    let model = SystemModel::new(init);

    undo_from_field_state(&model, "install-system-dependencies", &ctx());

    let s = model.snapshot();
    for pkg in ["python3-pip", "build-essential", "libpq-dev"] {
        assert!(!s.packages.contains(pkg), "'{pkg}' è nel delta: va purgato");
    }
    for pkg in ["git", "curl"] {
        assert!(
            s.packages.contains(pkg),
            "'{pkg}' era già installato prima di noi: NON va toccato"
        );
    }
}

#[test]
fn field_state_postgres_rehydrates_all_three_axes() {
    let mut init = fresh_state();
    init.packages.insert("postgresql".to_string());
    init.packages.insert("postgresql-contrib".to_string());
    init.svc_enabled.insert("postgresql".to_string());
    init.svc_active.insert("postgresql".to_string());
    let model = SystemModel::new(init);

    // `aggressive_rollback = false`: stop + disable sì, purge no (D3).
    let mut c = ctx();
    c.aggressive_rollback = false;
    undo_from_field_state(&model, "setup-postgres", &c);

    let s = model.snapshot();
    assert!(
        !s.svc_active.contains("postgresql"),
        "asse 'active' = CreatedByUs → il servizio va fermato"
    );
    assert!(
        !s.svc_enabled.contains("postgresql"),
        "asse 'enabled' = CreatedByUs → il servizio va disabilitato"
    );
    assert!(
        s.packages.contains("postgresql"),
        "asse 'installed' = CreatedByUs ma senza --aggressive-rollback il purge \
         non si fa: stop+disable sono reversibili, il purge no (D3)"
    );
}

#[test]
fn field_state_preexisting_prepare_opt_root_is_a_noop() {
    // `"Preexisting"` in forma di stringa nuda: il formato che il PreState
    // produce davvero. Reidratato, deve rendere l'undo inerte.
    let state = field_state();
    let record = &state.completed[0];
    assert_eq!(record.name, "prepare-opt-root");

    let model = SystemModel::new(fresh_state());
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let mut step = steps::step_by_name("prepare-opt-root", &make_ops).expect("factory");
    step.rehydrate(&record.snapshot).expect("rehydrate");

    let decoded: PreState =
        serde_json::from_value(record.snapshot.clone()).expect("PreState da stringa nuda");
    assert_eq!(decoded, PreState::Preexisting);

    step.undo(&ctx()).expect("undo");
    assert!(
        model.snapshot().paths.contains(&PathBuf::from(HOME)),
        "/opt/odoo era Preexisting: l'undo non deve rimuoverlo"
    );
}
