//! Il comando `rollback` (R4): rollback a partire dallo stato **persistito**.
//!
//! Il rollback in-process è già coperto da `tests/rollback_e2e.rs`. Qui si prova
//! l'altra metà — quella che finora non esisteva: il processo che annulla
//! un'installazione **che non ha eseguito**, ricostruendo gli step dal file di
//! stato. È lo scenario del Ctrl-C in campo, e quello della disinstallazione a
//! posteriori.
//!
//! Tutto gira su `SystemModel`: nessun comando reale, nessuna esecuzione del
//! binario.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clap::Parser;
use common::model::{ModelState, SystemModel};
use odoo_installer::cli::{Cli, Command};
use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::progress::NoopReporter;
use odoo_installer::rollback::{self, ConfirmationGate, InstallStatus, StepOutcome, UndoOutcome};
use odoo_installer::secret::Secret;
use odoo_installer::state::{InstallConfig, InstallState, StepRecord};
use odoo_installer::step::Step;
use odoo_installer::steps;
use odoo_installer::system_ops::SystemOps;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";
const UNIT: &str = "/etc/systemd/system/odoo18.service";

/// Catena reale ricostruibile dalla factory (stesse esclusioni di
/// `tests/rehydrate.rs`: fs diretto, temp reale, download reale).
const CHAIN: &[&str] = &[
    "create-odoo-user",
    "bootstrap-prerequisites",
    "install-system-dependencies",
    "setup-postgres",
    "create-db-role",
    "create-database",
    "clone-odoo-repo",
    "create-virtualenv",
    "generate-config",
    "initialize-odoo-database",
    "setup-systemd",
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
        ..Default::default()
    }
}

fn ctx(state_path: PathBuf) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        db_password: Secret::new("pg-segreto"),
        admin_passwd: Secret::new("admin-segreto"),
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(INSTALL),
        port: 8069,
        sudo_user: Some("alice".to_string()),
        state_path,
        aggressive_rollback: true,
        ..Default::default()
    }
}

fn chain_from_factory(model: &SystemModel, names: &[&str]) -> Vec<Box<dyn Step>> {
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    names
        .iter()
        .map(|name| {
            steps::step_by_name(name, &make_ops)
                .unwrap_or_else(|| panic!("la factory deve conoscere '{name}'"))
        })
        .collect()
}

/// Esegue un'installazione che **riesce** e lascia il file di stato sul disco.
///
/// Non chiama `mark_finished`: il file resta quindi come lo troverebbe un
/// `odoo-installer rollback` dopo un'**interruzione**. Il caso "installazione
/// conclusa e poi disinstallata" ha il suo test dedicato
/// (`a_successful_installation_leaves_a_state_that_can_still_be_rolled_back`).
fn install(model: &SystemModel, names: &[&str], ctx: &Context) {
    let mut steps = chain_from_factory(model, names);
    Installer::new()
        .execute(&mut steps, ctx)
        .expect("la catena deve arrivare in fondo");
}

/// Esegue il rollback dal file di stato su disco, su un modello dato.
fn rollback_from_disk(
    model: &SystemModel,
    state_path: &std::path::Path,
    dry_run: bool,
    aggressive: bool,
) -> (InstallState, rollback::RollbackReport) {
    let state = InstallState::load(state_path).expect("load dello stato");
    let config = state
        .config
        .clone()
        .expect("lo stato deve portare la configurazione");
    let ctx = config.to_context(dry_run, aggressive, state_path.to_path_buf());
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report = rollback::rollback_from_state(&state, &ctx, &make_ops, &NoopReporter);
    (state, report)
}

// --- La factory copre la sequenza canonica ----------------------------------

#[test]
fn the_factory_covers_the_whole_canonical_sequence() {
    // Il rollback da disco può ricostruire solo ciò che la factory conosce. Uno
    // step aggiunto a `build_steps` e dimenticato in `step_by_name` non
    // romperebbe nulla in installazione: si scoprirebbe mesi dopo, su una
    // macchina cliente, come "quel pezzo non è stato rimosso". Questo test lo
    // fa fallire subito.
    let make_ops = steps::real_ops;
    for name in steps::canonical_step_names() {
        assert!(
            steps::step_by_name(&name, &make_ops).is_some(),
            "'{name}' è nella sequenza canonica ma la factory non sa costruirlo: \
             il rollback da disco non potrebbe annullarlo"
        );
    }
}

#[test]
fn the_factory_rejects_an_unknown_name() {
    let make_ops = steps::real_ops;
    assert!(steps::step_by_name("passo-inventato", &make_ops).is_none());
}

// --- Rollback da stato completo ---------------------------------------------

#[test]
fn rollback_from_a_complete_state_returns_the_system_to_virgin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let install_model = SystemModel::new(fresh_state());
    let initial = install_model.snapshot();

    install(&install_model, CHAIN, &ctx(state_path.clone()));
    let after_install = install_model.snapshot();
    assert_ne!(
        after_install, initial,
        "l'installazione ha mutato il sistema"
    );

    // Il rollback gira su un processo **diverso**: modello nuovo, costruito
    // dallo stato che l'installazione ha lasciato, e step ricostruiti dal solo
    // file JSON. Nessun oggetto sopravvive fra le due fasi.
    let rollback_model = SystemModel::new(after_install);
    let (state, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert_eq!(state.completed.len(), CHAIN.len());
    assert!(
        report.is_clean(),
        "nessun residuo atteso, trovati: {:?}",
        report.residue()
    );
    assert_eq!(report.undone(), CHAIN.len());
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "dopo il rollback da disco il sistema è tornato al vergine"
    );
    assert_eq!(
        rollback_model
            .snapshot()
            .file_contents
            .get(&PathBuf::from(BASHRC))
            .map(String::as_str),
        Some(BASHRC_ORIG),
        "il .bashrc dell'utente torna byte-per-byte com'era"
    );
}

#[test]
fn the_undo_order_is_the_reverse_of_the_execution_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, CHAIN, &ctx(state_path.clone()));

    let state = InstallState::load(&state_path).expect("load");
    let plan: Vec<&str> = rollback::undo_plan(&state);
    let expected: Vec<&str> = CHAIN.iter().rev().copied().collect();
    assert_eq!(plan, expected, "invariante 2: undo in ordine inverso");

    // E il report rispetta quell'ordine (è ciò che l'utente vede).
    let (_, report) = rollback_from_disk(&model, &state_path, true, false);
    let executed: Vec<&str> = report.outcomes.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(executed, expected);
}

// --- Rollback da stato parziale (lo scenario Ctrl-C) ------------------------

#[test]
fn rollback_from_a_partial_state_undoes_exactly_those_steps() {
    // Interruzione a metà: l'installazione arriva al quinto step e il processo
    // muore. Il file di stato che resta sul disco è esattamente quello che il
    // motore aveva scritto fin lì — cinque record, non tredici.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");

    // Un artefatto **del cliente** che un undo successivo toccherebbe se
    // girasse: l'unit systemd. `setup-systemd` non è fra gli step completati,
    // quindi il suo undo non deve nemmeno essere tentato.
    let mut init = fresh_state();
    init.paths.insert(PathBuf::from(UNIT));
    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let partial: Vec<&str> = CHAIN.iter().take(5).copied().collect();
    install(&model, &partial, &ctx(state_path.clone()));

    let state = InstallState::load(&state_path).expect("load");
    assert_eq!(state.completed.len(), 5, "lo stato registra solo i 5 step");
    assert_eq!(
        rollback::install_status(&state),
        InstallStatus::Interrupted {
            done: 5,
            total: steps::canonical_step_names().len()
        },
        "il comando deve saper dire all'utente che l'installazione era a metà"
    );

    let rollback_model = SystemModel::new(model.snapshot());
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert!(report.is_clean(), "residui: {:?}", report.residue());
    assert_eq!(report.outcomes.len(), 5, "annullati esattamente i 5 step");
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "il rollback pulisce quei 5 step e nient'altro"
    );
    assert!(
        rollback_model
            .snapshot()
            .paths
            .contains(&PathBuf::from(UNIT)),
        "l'unit del cliente non è nostra: nessuno step completato la riguardava"
    );
}

// --- Stato assente ----------------------------------------------------------

#[test]
fn a_missing_state_file_is_not_an_error() {
    // Condizione normale: macchina pulita, o rollback già eseguito. Deve
    // risolversi in "niente da fare", non in un errore né in un panic.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("non-esiste.json");

    let state = InstallState::load(&missing).expect("un file assente non è un errore");
    assert!(state.completed.is_empty());
    assert!(rollback::undo_plan(&state).is_empty());

    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report =
        rollback::rollback_from_state(&state, &ctx(missing).clone(), &make_ops, &NoopReporter);

    assert!(report.outcomes.is_empty());
    assert!(report.is_clean());
    assert_eq!(model.snapshot(), initial, "nulla è stato toccato");
}

// --- Dry-run ----------------------------------------------------------------

#[test]
fn a_dry_run_rollback_mutates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, CHAIN, &ctx(state_path.clone()));
    let after_install = model.snapshot();

    let rollback_model = SystemModel::new(after_install.clone());
    let (state, report) = rollback_from_disk(&rollback_model, &state_path, true, true);

    assert_eq!(
        rollback_model.snapshot(),
        after_install,
        "--dry-run: il sistema non deve cambiare di un byte"
    );
    // Il piano è comunque completo: il dry-run serve a mostrare *cosa* farebbe.
    assert_eq!(report.outcomes.len(), CHAIN.len());
    assert_eq!(rollback::undo_plan(&state).len(), CHAIN.len());
}

// --- Best-effort e report dei residui (A1.3) --------------------------------

#[test]
fn a_failing_undo_does_not_block_the_others_and_ends_up_in_the_report() {
    // Invariante 3 anche da disco. Qui la home di `alice` non è più risolvibile
    // (utente rimosso nel frattempo): gli step che scrivono nella sua home non
    // possono annullare, ma tutto il resto deve essere ripulito lo stesso — e
    // ciò che è rimasto deve finire nel report, non solo in un warning.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, CHAIN, &ctx(state_path.clone()));

    let mut broken = model.snapshot();
    broken.sudo_home = None;
    let rollback_model = SystemModel::new(broken);
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert!(!report.is_clean(), "i due step sulla home devono fallire");
    let failed: Vec<&str> = report
        .residue()
        .iter()
        .map(|o: &&StepOutcome| o.name.as_str())
        .collect();
    assert!(failed.contains(&"write-control-script"), "{failed:?}");
    assert!(failed.contains(&"patch-bashrc"), "{failed:?}");
    for outcome in report.residue() {
        assert!(
            matches!(outcome.outcome, UndoOutcome::Failed(_)),
            "gli undo falliti vanno classificati come tali: {outcome:?}"
        );
    }

    // Best-effort: il resto è stato ripulito comunque.
    let s = rollback_model.snapshot();
    assert!(!s.users.contains("odoo"), "l'utente odoo è stato rimosso");
    assert!(!s.pg_dbs.contains("odoo"), "il database è stato droppato");
    assert!(
        !s.paths.contains(&PathBuf::from(INSTALL)),
        "i sorgenti sono stati rimossi"
    );
    assert!(
        report.undone() >= CHAIN.len() - 2,
        "solo i due step bloccati devono risultare non completati"
    );
}

#[test]
fn an_unknown_step_name_is_reported_instead_of_aborting_the_rollback() {
    // Stato scritto da una versione con step che questo binario non conosce.
    // Non è un motivo per rinunciare a ripulire tutto il resto.
    let model = SystemModel::new({
        let mut s = fresh_state();
        s.users.insert("odoo".to_string());
        s
    });
    let state = InstallState {
        completed: vec![
            StepRecord {
                name: "create-odoo-user".to_string(),
                snapshot: serde_json::json!({
                    "user_prestate": "CreatedByUs",
                    "home_original_owner": null
                }),
            },
            StepRecord {
                name: "step-di-una-versione-futura".to_string(),
                snapshot: serde_json::Value::Null,
            },
        ],
        config: Some(config_fixture()),
        finished: false,
    };

    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let c =
        state
            .config
            .clone()
            .expect("config")
            .to_context(false, false, PathBuf::from("/dev/null"));
    let report = rollback::rollback_from_state(&state, &c, &make_ops, &NoopReporter);

    assert_eq!(report.residue().len(), 1);
    assert_eq!(report.residue()[0].name, "step-di-una-versione-futura");
    assert_eq!(report.residue()[0].outcome, UndoOutcome::Unknown);
    assert!(
        !model.snapshot().users.contains("odoo"),
        "lo step conosciuto è stato annullato lo stesso"
    );
}

#[test]
fn a_corrupt_snapshot_skips_the_undo_and_is_reported() {
    // Fail-closed: uno snapshot illeggibile NON deve far girare l'undo con uno
    // stato di default. Su `create-database` uno stato inventato potrebbe
    // significare droppare un database del cliente.
    let mut init = fresh_state();
    init.pg_dbs.insert("odoo".to_string());
    let model = SystemModel::new(init);

    let state = InstallState {
        completed: vec![StepRecord {
            name: "create-database".to_string(),
            snapshot: serde_json::json!({ "non": "un PreState" }),
        }],
        config: Some(config_fixture()),
        finished: false,
    };
    let c = config_fixture().to_context(false, false, PathBuf::from("/dev/null"));
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report = rollback::rollback_from_state(&state, &c, &make_ops, &NoopReporter);

    assert_eq!(report.undone(), 0);
    assert!(matches!(
        report.residue()[0].outcome,
        UndoOutcome::NotRehydrated(_)
    ));
    assert!(
        model.snapshot().pg_dbs.contains("odoo"),
        "snapshot illeggibile → nessun drop: il dubbio si risolve non agendo"
    );
}

// --- Protezione anti-drop attraverso il disco -------------------------------

fn config_fixture() -> InstallConfig {
    InstallConfig::from_context(&ctx(PathBuf::from("/dev/null")))
}

#[test]
fn a_preexisting_database_is_not_dropped_by_a_rollback_from_disk() {
    // La protezione critica, esercitata dove è più insidiosa: il rollback gira
    // in un processo che non ha mai visto quel database "prima". L'unica cosa
    // che lo salva è il `PreState` riletto dal disco.
    let mut init = fresh_state();
    init.pg_dbs.insert("odoo".to_string()); // dati reali del cliente
    init.pg_roles.insert("odoo".to_string());
    let model = SystemModel::new(init);

    let state = InstallState {
        completed: vec![
            StepRecord {
                name: "create-db-role".to_string(),
                snapshot: serde_json::json!("CreatedByUs"),
            },
            StepRecord {
                name: "create-database".to_string(),
                snapshot: serde_json::json!("Preexisting"),
            },
        ],
        config: Some(config_fixture()),
        finished: false,
    };
    let c = config_fixture().to_context(false, true, PathBuf::from("/dev/null"));
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report = rollback::rollback_from_state(&state, &c, &make_ops, &NoopReporter);

    assert!(report.is_clean());
    let s = model.snapshot();
    assert!(
        s.pg_dbs.contains("odoo"),
        "snapshot=Preexisting → il database del cliente NON va droppato, mai"
    );
    assert!(
        !s.pg_roles.contains("odoo"),
        "il ruolo era invece CreatedByUs: quello va rimosso"
    );
}

#[test]
fn the_preexisting_marker_survives_the_full_disk_roundtrip() {
    // Stessa protezione, ma col percorso completo: installazione reale su un
    // sistema dove il database esiste già → snapshot → file su disco →
    // rilettura → undo. Nessun valore scritto a mano.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let mut init = fresh_state();
    init.pg_dbs.insert("odoo".to_string());
    let model = SystemModel::new(init);
    let initial = model.snapshot();

    install(
        &model,
        &["create-db-role", "create-database"],
        &ctx(state_path.clone()),
    );

    let persisted = InstallState::load(&state_path).expect("load");
    let db_record = persisted
        .completed
        .iter()
        .find(|r| r.name == "create-database")
        .expect("record del database");
    assert_eq!(
        db_record.snapshot,
        serde_json::json!("Preexisting"),
        "il DB esisteva prima di noi: è ciò che il file di stato deve dire"
    );

    let rollback_model = SystemModel::new(model.snapshot());
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);
    assert!(report.is_clean());
    assert!(
        rollback_model.snapshot().pg_dbs.contains("odoo"),
        "attraverso serializzazione e reidratazione, l'anti-drop regge"
    );
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "e il resto torna com'era"
    );
}

// --- Contenuto del file di stato --------------------------------------------

#[test]
fn the_state_file_carries_the_config_and_no_passwords() {
    // La configurazione persistita è ciò che dà al rollback l'identità degli
    // artefatti. Le password no: nessun undo le usa, quindi non vengono scritte.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, &["create-odoo-user"], &ctx(state_path.clone()));

    let raw = std::fs::read_to_string(&state_path).expect("file di stato");
    assert!(
        !raw.contains("admin-segreto"),
        "la password admin non deve finire nel file di stato"
    );
    assert!(
        !raw.contains("pg-segreto"),
        "la password del ruolo PostgreSQL non deve finire nel file di stato"
    );

    let config = InstallState::load(&state_path)
        .expect("load")
        .config
        .expect("la configurazione deve essere persistita");
    assert_eq!(config.db_name, "odoo");
    assert_eq!(config.odoo_user, "odoo");
    assert_eq!(config.install_dir, PathBuf::from(INSTALL));
    assert_eq!(config.sudo_user.as_deref(), Some("alice"));
}

#[test]
fn install_status_recognises_a_complete_installation() {
    let names = steps::canonical_step_names();
    let state = InstallState {
        completed: names
            .iter()
            .map(|n| StepRecord {
                name: n.clone(),
                snapshot: serde_json::Value::Null,
            })
            .collect(),
        config: Some(config_fixture()),
        // Deliberatamente `false`: qui si prova il **ripiego** sul conteggio,
        // che copre gli stati scritti prima che il flag esistesse.
        finished: false,
    };
    assert_eq!(
        rollback::install_status(&state),
        InstallStatus::Complete { steps: names.len() },
        "tutti gli step canonici presenti = installazione da disinstallare, \
         non residui da ripulire"
    );
}

// --- A-R5-1: il manifesto di disinstallazione sopravvive al successo --------

#[test]
fn a_successful_installation_leaves_a_state_that_can_still_be_rolled_back() {
    // Il caso d'uso principale del comando: disinstallare un'istanza
    // **funzionante**. Fino a R5 era impossibile — `main` cancellava lo stato a
    // successo avvenuto, e `odoo-installer rollback` rispondeva "nessuna
    // installazione da annullare" su un sistema pieno di artefatti nostri.
    //
    // Qui l'installazione arriva in fondo, viene marcata conclusa, e il rollback
    // riparte da quel file: il sistema deve tornare vergine.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    let c = ctx(state_path.clone());
    let mut steps_vec = chain_from_factory(&model, CHAIN);
    let mut installer = Installer::new();
    installer
        .execute(&mut steps_vec, &c)
        .expect("la catena deve arrivare in fondo");
    installer
        .mark_finished(&c)
        .expect("marcatura di fine installazione");

    // Lo stato è ancora lì, e si dichiara completo.
    let state = InstallState::load(&state_path).expect("lo stato deve sopravvivere al successo");
    assert!(state.finished, "l'installazione riuscita marca lo stato");
    assert_eq!(
        rollback::install_status(&state),
        InstallStatus::Complete { steps: CHAIN.len() },
        "il flag ha la precedenza sul conteggio: questa catena è più corta di \
         quella canonica ma l'installazione è comunque completa"
    );

    // E il rollback lo consuma: disinstallazione di un'istanza funzionante.
    let rollback_model = SystemModel::new(model.snapshot());
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);
    assert!(report.is_clean(), "residui: {:?}", report.residue());
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "un'installazione completata deve poter essere disinstallata per intero"
    );
}

#[test]
fn marking_finished_writes_nothing_in_dry_run() {
    // Una preview non lascia artefatti, nemmeno il manifesto.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let mut c = ctx(state_path.clone());
    c.dry_run = true;

    Installer::new().mark_finished(&c).expect("mark_finished");
    assert!(
        !state_path.exists(),
        "in dry-run non deve essere scritto alcun file di stato"
    );
}

// --- Conferma e retrocompatibilità della CLI --------------------------------

#[test]
fn the_confirmation_gate_protects_a_destructive_operation() {
    use ConfirmationGate::*;
    // Interattivo, senza --yes → si chiede.
    assert_eq!(rollback::confirmation_gate(false, false, true), Ask);
    // Non interattivo, senza --yes → si rifiuta: uno script non deve poter
    // disinstallare per default.
    assert_eq!(
        rollback::confirmation_gate(false, false, false),
        RefuseNonInteractive
    );
    // --yes è una conferma esplicita, con o senza terminale.
    assert_eq!(rollback::confirmation_gate(false, true, false), Proceed);
    assert_eq!(rollback::confirmation_gate(false, true, true), Proceed);
    // --dry-run non muta nulla: non c'è nulla da confermare.
    assert_eq!(rollback::confirmation_gate(true, false, false), Proceed);
}

#[test]
fn the_bare_command_still_installs() {
    // Retrocompatibilità: l'uso storico non cambia. Nessun sottocomando e le
    // stesse opzioni di prima.
    let cli = Cli::parse_from(["odoo-installer"]);
    assert!(cli.command.is_none(), "nessun sottocomando = installazione");

    let cli = Cli::parse_from([
        "odoo-installer",
        "--version",
        "18",
        "--db-name",
        "fatturazione",
        "--with-nginx",
        "--dry-run",
    ]);
    assert!(cli.command.is_none());
    assert_eq!(cli.version.as_deref(), Some("18"));
    assert_eq!(cli.db_name.as_deref(), Some("fatturazione"));
    assert!(cli.with_nginx);
    assert!(cli.dry_run);
}

#[test]
fn rollback_and_its_uninstall_alias_parse_with_their_options() {
    let cli = Cli::parse_from(["odoo-installer", "rollback"]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("atteso il sottocomando rollback");
    };
    assert!(
        args.state.is_none(),
        "senza --state il percorso lo risolve `state::resolve_state_path`"
    );
    assert!(!args.dry_run && !args.yes && !args.aggressive_rollback);

    let cli = Cli::parse_from([
        "odoo-installer",
        "rollback",
        "--state",
        "/tmp/altro-stato.json",
        "--dry-run",
        "--aggressive-rollback",
        "-y",
    ]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("atteso il sottocomando rollback");
    };
    assert_eq!(
        args.state.as_deref(),
        Some(std::path::Path::new("/tmp/altro-stato.json"))
    );
    assert!(args.dry_run && args.yes && args.aggressive_rollback);

    // L'alias `uninstall` è lo stesso comando.
    let cli = Cli::parse_from(["odoo-installer", "uninstall", "--yes"]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("l'alias uninstall deve mappare su rollback");
    };
    assert!(args.yes);
}

// --- A3.3: il messaggio di hard-stop indica il comando reale ----------------

#[test]
fn the_init_hard_stop_points_at_the_rollback_command() {
    // R0 aveva messo "la rimozione automatica arriverà col comando rollback (in
    // arrivo)". Il comando ora esiste: il messaggio deve nominarlo per quello
    // che è, altrimenti l'utente resta bloccato con in mano un suggerimento
    // scaduto.
    let mut init = fresh_state();
    init.pg_dbs.insert("odoo".to_string()); // DB preesistente, schema assente
    let model = SystemModel::new(init);
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let mut step = steps::step_by_name("initialize-odoo-database", &make_ops).expect("factory");

    let c = ctx(PathBuf::from("/dev/null"));
    let err = step
        .snapshot(&c)
        .expect_err("hard-stop atteso su DB preesistente");
    let msg = err.to_string();

    assert!(
        msg.contains("odoo-installer rollback"),
        "il messaggio deve indicare il comando reale, non una promessa: {msg}"
    );
    assert!(
        !msg.contains("in arrivo"),
        "niente più 'in arrivo': il comando c'è: {msg}"
    );
    assert!(
        msg.contains("dropdb"),
        "resta anche l'alternativa manuale: {msg}"
    );
}
