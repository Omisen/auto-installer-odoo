//! Test di rollback **end-to-end** (Fase 12, G7): la prova della promessa
//! "chirurgico" a livello di sistema.
//!
//! Si parte da uno stato iniziale del [`SystemModel`], si esegue la sequenza
//! reale di step, si inietta un fallimento, e si verifica che dopo il rollback
//! lo stato sia **identico** all'iniziale — e che le risorse preesistenti del
//! cliente sopravvivano intatte.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::secret::Secret;
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::AptPackagesStep;
use odoo_installer::steps::clone_odoo_repo::CloneOdooRepo;
use odoo_installer::steps::create_database::CreateDatabase;
use odoo_installer::steps::create_db_role::CreateDbRole;
use odoo_installer::steps::create_odoo_user::CreateOdooUser;
use odoo_installer::steps::create_virtualenv::CreateVirtualenv;
use odoo_installer::steps::generate_config::GenerateConfig;
use odoo_installer::steps::initialize_odoo_database::InitializeOdooDatabase;
use odoo_installer::steps::noop::NoopStep;
use odoo_installer::steps::patch_bashrc::PatchBashrc;
use odoo_installer::steps::setup_systemd::SetupSystemd;
use odoo_installer::steps::write_control_script::WriteControlScript;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}
fn paths(items: &[&str]) -> HashSet<PathBuf> {
    items.iter().map(PathBuf::from).collect()
}

/// Stato iniziale "macchina fresca": /opt/odoo e la home utente esistono, il
/// .bashrc dell'utente ha il suo contenuto, nient'altro di nostro.
fn fresh_state() -> ModelState {
    let mut contents = HashMap::new();
    contents.insert(PathBuf::from(BASHRC), BASHRC_ORIG.to_string());
    ModelState {
        paths: paths(&[HOME, SUDO_HOME, BASHRC]),
        file_contents: contents,
        packages: set(&["coreutils"]),
        sudo_home: Some(SUDO_HOME.to_string()),
        ..Default::default()
    }
}

fn ctx(state_path: PathBuf, aggressive: bool) -> Context {
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
        with_nginx: false,
        dry_run: false,
        aggressive_rollback: aggressive,
        sudo_user: Some("alice".to_string()),
        state_path,
        ..Default::default()
    }
}

/// La sequenza reale (mockabile) di step, condividendo lo stesso modello.
/// PrepareOptRoot è escluso (usa `std::fs` diretto; il suo ciclo su /opt/odoo è
/// coperto dai test di Fase 2); qui /opt/odoo è parte dello stato iniziale.
fn chain(model: &SystemModel) -> Vec<Box<dyn Step>> {
    vec![
        Box::new(CreateOdooUser::with_ops(model.boxed())),
        Box::new(AptPackagesStep::odoo_dependencies_with_ops(model.boxed())),
        Box::new(SetupPostgres::with_ops(model.boxed())),
        Box::new(CreateDbRole::with_ops(model.boxed())),
        Box::new(CreateDatabase::with_ops(model.boxed())),
        Box::new(CloneOdooRepo::with_ops(model.boxed())),
        Box::new(CreateVirtualenv::with_ops(model.boxed())),
        Box::new(GenerateConfig::with_ops(model.boxed())),
        Box::new(InitializeOdooDatabase::with_ops(model.boxed())),
        Box::new(SetupSystemd::with_ops(model.boxed())),
        Box::new(WriteControlScript::with_ops(model.boxed())),
        Box::new(PatchBashrc::with_ops(model.boxed())),
    ]
}

use odoo_installer::steps::setup_postgres::SetupPostgres;

#[test]
fn full_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // Sequenza completa + uno step che fallisce alla fine → rollback di tutto.
    let mut steps = chain(&model);
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), /* aggressive */ true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err(), "il fallimento innesca il rollback");

    assert_eq!(model.snapshot(), initial, "dopo il rollback il sistema è tornato al vergine");
    // Il .bashrc dell'utente è byte-per-byte come prima.
    assert_eq!(
        model.snapshot().file_contents.get(&PathBuf::from(BASHRC)).map(String::as_str),
        Some(BASHRC_ORIG)
    );
}

#[test]
fn mid_chain_failure_returns_to_virgin_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    // Fallimento "a CloneOdooRepo": i primi 5 step (utente, deps, postgres,
    // ruolo, DB) completano, poi si inietta il fallimento.
    let mut steps: Vec<Box<dyn Step>> = chain(&model).into_iter().take(5).collect();
    steps.push(Box::new(NoopStep::new("boom").fail_on_run()));

    let ctx = ctx(dir.path().join("state.json"), true);
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());

    assert_eq!(model.snapshot(), initial, "utente/deps/postgres/ruolo/DB creati devono sparire");
}

#[test]
fn preexisting_resources_survive_rollback() {
    // Stato iniziale con RISORSE DEL CLIENTE: PostgreSQL installato+attivo, un DB
    // 'odoo' che esiste già (dati del cliente!). L'init hard-stoppa (DB non
    // nostro), la catena fallisce, e il rollback NON tocca ciò che era già lì.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut init = fresh_state();
    init.packages.insert("postgresql".to_string());
    init.packages.insert("postgresql-contrib".to_string());
    init.svc_enabled.insert("postgresql".to_string());
    init.svc_active.insert("postgresql".to_string());
    init.pg_dbs.insert("odoo".to_string()); // DB preesistente del cliente

    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let mut steps = chain(&model);
    let ctx = ctx(dir.path().join("state.json"), false);
    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);
    assert!(result.is_err(), "l'hard-stop init deve fermare la catena su DB preesistente");

    let final_state = model.snapshot();
    // Tutto tornato com'era.
    assert_eq!(final_state, initial, "le risorse preesistenti restano, le nostre spariscono");
    // Verifiche esplicite delle protezioni critiche a livello di catena:
    assert!(final_state.pg_dbs.contains("odoo"), "il DB del cliente NON deve essere droppato");
    assert!(final_state.packages.contains("postgresql"), "PostgreSQL preinstallato resta");
    assert!(final_state.svc_active.contains("postgresql"), "il servizio già attivo resta attivo (D4)");
    assert!(final_state.paths.contains(&PathBuf::from(HOME)), "/opt/odoo preesistente resta");
}
