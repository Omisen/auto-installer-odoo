//! the `rollback` command: undoing from the **persisted** state.
//!
//! the in-process rollback is covered elsewhere. here is the other half: a
//! process undoing an installation **it never ran**, rebuilding the steps from
//! the state file. the field's Ctrl-C scenario, and the after-the-fact
//! uninstall.
//!
//! everything runs on the model: no real command, no binary executed.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clap::Parser;
use common::model::{ModelState, SystemModel};
use invok::cli::{Cli, Command};
use invok::context::Context;
use invok::engine::Installer;
use invok::progress::NoopReporter;
use invok::rollback::{self, ConfirmationGate, InstallStatus, StepOutcome, UndoOutcome};
use invok::secret::Secret;
use invok::state::{InstallConfig, InstallState, StepRecord};
use invok::step::Step;
use invok::steps;
use invok::system_ops::SystemOps;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";
const UNIT: &str = "/etc/systemd/system/odoo18.service";

/// the real chain the factory can rebuild, with the same exclusions as the
/// rehydration tests: direct filesystem, real temporaries, real downloads.
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
                .unwrap_or_else(|| panic!("the factory must know '{name}'"))
        })
        .collect()
}

/// runs an installation that **succeeds** and leaves the state file on disk.
///
/// it does not mark the state finished, so the file looks as a rollback would
/// find it after an **interruption**; the finished case has its own test. the
/// canonical sequence does not depend on the family, but building it still
/// needs an ops factory: the production one, which runs no command in its
/// constructors.
fn canonical_len() -> usize {
    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("the Debian family has a backend");
    steps::canonical_step_names(&make_ops).len()
}

fn install(model: &SystemModel, names: &[&str], ctx: &Context) {
    let mut steps = chain_from_factory(model, names);
    Installer::new()
        .execute(&mut steps, ctx)
        .expect("the chain must reach the end");
}

/// runs the rollback from the state file on disk, against a given model.
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
        .expect("the state must carry the configuration");
    let ctx = config.to_context(dry_run, aggressive, state_path.to_path_buf());
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report = rollback::rollback_from_state(&state, &ctx, &make_ops, &NoopReporter);
    (state, report)
}

// --- the factory covers the canonical sequence ------------------------------

#[test]
fn the_factory_covers_the_whole_canonical_sequence() {
    // the disk rollback can only rebuild what the factory knows. a step added
    // to the sequence and forgotten in the factory breaks nothing during an
    // installation: it would surface months later on a customer machine as
    // "that piece was not removed".
    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("the Debian family has a backend");
    for name in steps::canonical_step_names(&make_ops) {
        assert!(
            steps::step_by_name(&name, &make_ops).is_some(),
            "'{name}' is in the canonical sequence but the factory cannot build it: \
             the rollback from disk could not undo it"
        );
    }
}

#[test]
fn the_factory_rejects_an_unknown_name() {
    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("the Debian family has a backend");
    assert!(steps::step_by_name("passo-inventato", &make_ops).is_none());
}

// --- rollback from a complete state -----------------------------------------

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
        "the installation mutated the system"
    );

    // the rollback runs in a **different** process: a fresh model built from
    // the state the installation left, and steps rebuilt from the JSON alone.
    // no object survives between the two phases.
    let rollback_model = SystemModel::new(after_install);
    let (state, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert_eq!(state.completed.len(), CHAIN.len());
    assert!(
        report.is_clean(),
        "no leftovers expected, found: {:?}",
        report.residue()
    );
    assert_eq!(report.undone(), CHAIN.len());
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "after the rollback from disk the system is virgin again"
    );
    assert_eq!(
        rollback_model
            .snapshot()
            .file_contents
            .get(&PathBuf::from(BASHRC))
            .map(String::as_str),
        Some(BASHRC_ORIG),
        "the user's .bashrc comes back byte for byte"
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
    assert_eq!(plan, expected, "invariant 2: undos in reverse order");

    // and the report respects that order, which is what the user sees.
    let (_, report) = rollback_from_disk(&model, &state_path, true, false);
    let executed: Vec<&str> = report.outcomes.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(executed, expected);
}

// --- rollback from a partial state (the Ctrl-C scenario) --------------------

#[test]
fn rollback_from_a_partial_state_undoes_exactly_those_steps() {
    // interrupted halfway: the state left on disk is exactly what the engine
    // had written so far, five records and not the whole chain.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");

    // a **customer** artifact a later undo would touch if it ran: its step is
    // not among the completed ones, so that undo must not even be attempted.
    let mut init = fresh_state();
    init.paths.insert(PathBuf::from(UNIT));
    let model = SystemModel::new(init);
    let initial = model.snapshot();

    let partial: Vec<&str> = CHAIN.iter().take(5).copied().collect();
    install(&model, &partial, &ctx(state_path.clone()));

    let state = InstallState::load(&state_path).expect("load");
    assert_eq!(
        state.completed.len(),
        5,
        "the state records only the five steps"
    );
    assert_eq!(
        rollback::install_status(&state, canonical_len()),
        InstallStatus::Interrupted {
            done: 5,
            total: canonical_len()
        },
        "the command must be able to tell the user the installation was halfway"
    );

    let rollback_model = SystemModel::new(model.snapshot());
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert!(report.is_clean(), "residui: {:?}", report.residue());
    assert_eq!(report.outcomes.len(), 5, "annullati esattamente i 5 step");
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "the rollback cleans those five steps and nothing else"
    );
    assert!(
        rollback_model
            .snapshot()
            .paths
            .contains(&PathBuf::from(UNIT)),
        "the customer's unit is not ours: no completed step concerned it"
    );
}

// --- no state ---------------------------------------------------------------

#[test]
fn a_missing_state_file_is_not_an_error() {
    // a normal condition — clean machine, or a rollback already done. it must
    // resolve to "nothing to do", not an error and not a panic.
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("non-esiste.json");

    let state = InstallState::load(&missing).expect("a missing file is not an error");
    assert!(state.completed.is_empty());
    assert!(rollback::undo_plan(&state).is_empty());

    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report =
        rollback::rollback_from_state(&state, &ctx(missing).clone(), &make_ops, &NoopReporter);

    assert!(report.outcomes.is_empty());
    assert!(report.is_clean());
    assert_eq!(model.snapshot(), initial, "nothing was touched");
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
        "--dry-run: the system must not change by a byte"
    );
    // the plan is still complete: a dry run exists to show *what* it would do.
    assert_eq!(report.outcomes.len(), CHAIN.len());
    assert_eq!(rollback::undo_plan(&state).len(), CHAIN.len());
}

// --- best-effort, and the leftovers report (A1.3) ---------------------------

#[test]
fn a_failing_undo_does_not_block_the_others_and_ends_up_in_the_report() {
    // invariant 3 from disk too. a user's home is no longer resolvable, so the
    // steps writing there cannot undo — but everything else must still be
    // cleaned, and what remains must reach the report, not just a warning.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, CHAIN, &ctx(state_path.clone()));

    let mut broken = model.snapshot();
    broken.sudo_home = None;
    let rollback_model = SystemModel::new(broken);
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);

    assert!(!report.is_clean(), "the two steps on the home must fail");
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
            "failed undos must be classified as such: {outcome:?}"
        );
    }

    // best-effort: the rest was cleaned anyway.
    let s = rollback_model.snapshot();
    assert!(!s.users.contains("odoo"), "the odoo user was removed");
    assert!(!s.pg_dbs.contains("odoo"), "the database was dropped");
    assert!(
        !s.paths.contains(&PathBuf::from(INSTALL)),
        "the sources were removed"
    );
    assert!(
        report.undone() >= CHAIN.len() - 2,
        "only the two blocked steps may come out uncompleted"
    );
}

#[test]
fn an_unknown_step_name_is_reported_instead_of_aborting_the_rollback() {
    // a state written by a version with steps this binary does not know. not a
    // reason to give up on everything else.
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
                name: "step-from-a-future-version".to_string(),
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
    assert_eq!(report.residue()[0].name, "step-from-a-future-version");
    assert_eq!(report.residue()[0].outcome, UndoOutcome::Unknown);
    assert!(
        !model.snapshot().users.contains("odoo"),
        "the known step was undone anyway"
    );
}

#[test]
fn a_corrupt_snapshot_skips_the_undo_and_is_reported() {
    // fail-closed: an unreadable snapshot must NOT run the undo with a default
    // state. on the database step an invented state could mean dropping a
    // customer's data.
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
        "unreadable snapshot means no drop: doubt is resolved by not acting"
    );
}

// --- the anti-drop protection, through the disk -----------------------------

fn config_fixture() -> InstallConfig {
    InstallConfig::from_context(&ctx(PathBuf::from("/dev/null")))
}

#[test]
fn a_preexisting_database_is_not_dropped_by_a_rollback_from_disk() {
    // the critical protection where it is most treacherous: the rollback runs
    // in a process that never saw that database "before". the only thing saving
    // it is the `PreState` read back from disk.
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
        "snapshot=Preexisting: the customer's database must NEVER be dropped"
    );
    assert!(
        !s.pg_roles.contains("odoo"),
        "the role, though, was CreatedByUs: that one is removed"
    );
}

#[test]
fn the_preexisting_marker_survives_the_full_disk_roundtrip() {
    // the same protection along the full path: a real installation on a system
    // where the database already exists, snapshot, file, re-read, undo. no
    // value written by hand.
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
        "the DB existed before us: that is what the state file must say"
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
        "and the rest comes back as it was"
    );
}

// --- the state file's contents ----------------------------------------------

#[test]
fn the_state_file_carries_the_config_and_no_passwords() {
    // the persisted config is what gives the rollback the artifacts' identity.
    // passwords are not: no undo uses them, so they are never written.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    install(&model, &["create-odoo-user"], &ctx(state_path.clone()));

    let raw = std::fs::read_to_string(&state_path).expect("state file");
    assert!(
        !raw.contains("admin-segreto"),
        "the admin password must not reach the state file"
    );
    assert!(
        !raw.contains("pg-segreto"),
        "the PostgreSQL role's password must not reach the state file"
    );

    let config = InstallState::load(&state_path)
        .expect("load")
        .config
        .expect("the configuration must be persisted");
    assert_eq!(config.db_name, "odoo");
    assert_eq!(config.odoo_user, "odoo");
    assert_eq!(config.install_dir, PathBuf::from(INSTALL));
    assert_eq!(config.sudo_user.as_deref(), Some("alice"));
}

#[test]
fn install_status_recognises_a_complete_installation() {
    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("the Debian family has a backend");
    let names = steps::canonical_step_names(&make_ops);
    let state = InstallState {
        completed: names
            .iter()
            .map(|n| StepRecord {
                name: n.clone(),
                snapshot: serde_json::Value::Null,
            })
            .collect(),
        config: Some(config_fixture()),
        // deliberately false: this exercises the **fallback** on the count,
        // which covers states written before the flag existed.
        finished: false,
    };
    assert_eq!(
        rollback::install_status(&state, canonical_len()),
        InstallStatus::Complete { steps: names.len() },
        "every canonical step present means an installation to uninstall, \
         not leftovers to clean up"
    );
}

// --- A-R5-1: the uninstall manifest survives success ------------------------

#[test]
fn a_successful_installation_leaves_a_state_that_can_still_be_rolled_back() {
    // the command's main use: uninstalling a **working** instance. until R5
    // that was impossible — the state was cleared on success, and the command
    // answered "nothing to undo" on a system full of our artifacts.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());
    let initial = model.snapshot();

    let c = ctx(state_path.clone());
    let mut steps_vec = chain_from_factory(&model, CHAIN);
    let mut installer = Installer::new();
    installer
        .execute(&mut steps_vec, &c)
        .expect("the chain must reach the end");
    installer
        .mark_finished(&c)
        .expect("marking the end of the installation");

    // the state is still there, and declares itself complete.
    let state = InstallState::load(&state_path).expect("the state must survive success");
    assert!(state.finished, "a successful installation marks the state");
    assert_eq!(
        rollback::install_status(&state, canonical_len()),
        InstallStatus::Complete { steps: CHAIN.len() },
        "the flag wins over the count: this chain is shorter than the canonical \
         one, yet the installation is complete"
    );

    // and the rollback consumes it.
    let rollback_model = SystemModel::new(model.snapshot());
    let (_, report) = rollback_from_disk(&rollback_model, &state_path, false, true);
    assert!(report.is_clean(), "residui: {:?}", report.residue());
    assert_eq!(
        rollback_model.snapshot(),
        initial,
        "a completed installation must be fully removable"
    );
}

#[test]
fn marking_finished_writes_nothing_in_dry_run() {
    // a preview leaves no artifacts, the manifest included.
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let mut c = ctx(state_path.clone());
    c.dry_run = true;

    Installer::new().mark_finished(&c).expect("mark_finished");
    assert!(!state_path.exists(), "a dry run must write no state file");
}

// --- confirmation, and the CLI's compatibility ------------------------------

#[test]
fn the_confirmation_gate_protects_a_destructive_operation() {
    use ConfirmationGate::*;
    // interactive without the flag: ask.
    assert_eq!(rollback::confirmation_gate(false, false, true), Ask);
    // non-interactive without it: refuse — a script must not uninstall by
    // default.
    assert_eq!(
        rollback::confirmation_gate(false, false, false),
        RefuseNonInteractive
    );
    // the flag is an explicit confirmation, terminal or not.
    assert_eq!(rollback::confirmation_gate(false, true, false), Proceed);
    assert_eq!(rollback::confirmation_gate(false, true, true), Proceed);
    // a dry run mutates nothing, so there is nothing to confirm.
    assert_eq!(rollback::confirmation_gate(true, false, false), Proceed);
}

#[test]
fn the_bare_command_still_installs() {
    // compatibility: no subcommand and the same options as before.
    let cli = Cli::parse_from(["invok"]);
    assert!(cli.command.is_none(), "no subcommand means an installation");

    let cli = Cli::parse_from([
        "invok",
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
    assert!(!cli.force, "--force is off unless passed");
}

/// the force flag is the explicit way out of the refusal on an existing
/// manifest (A-V3-1), and must not have disturbed any existing parsing.
#[test]
fn force_is_an_install_flag_and_defaults_to_off() {
    let cli = Cli::parse_from(["invok", "--force"]);
    assert!(
        cli.command.is_none(),
        "it stays an installation, not a subcommand"
    );
    assert!(cli.force);
    assert!(!cli.dry_run && !cli.aggressive_rollback);
}

#[test]
fn rollback_and_its_uninstall_alias_parse_with_their_options() {
    let cli = Cli::parse_from(["invok", "rollback"]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("the rollback subcommand was expected");
    };
    assert!(
        args.state.is_none(),
        "without --state the path is resolved by `state::resolve_state_path`"
    );
    assert!(!args.dry_run && !args.yes && !args.aggressive_rollback);

    let cli = Cli::parse_from([
        "invok",
        "rollback",
        "--state",
        "/tmp/altro-stato.json",
        "--dry-run",
        "--aggressive-rollback",
        "-y",
    ]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("the rollback subcommand was expected");
    };
    assert_eq!(
        args.state.as_deref(),
        Some(std::path::Path::new("/tmp/altro-stato.json"))
    );
    assert!(args.dry_run && args.yes && args.aggressive_rollback);

    // the alias is the same command.
    let cli = Cli::parse_from(["invok", "uninstall", "--yes"]);
    let Some(Command::Rollback(args)) = &cli.command else {
        panic!("the uninstall alias must map onto rollback");
    };
    assert!(args.yes);
}

// --- A3.3: the hard-stop message names the real command ---------------------

#[test]
fn the_init_hard_stop_points_at_the_rollback_command() {
    // the message used to promise a command that did not exist yet. it does
    // now, and must be named for what it is or the reader is left holding an
    // expired hint.
    let mut init = fresh_state();
    init.pg_dbs.insert("odoo".to_string()); // DB preesistente, schema assente
    let model = SystemModel::new(init);
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let mut step = steps::step_by_name("initialize-odoo-database", &make_ops).expect("factory");

    let c = ctx(PathBuf::from("/dev/null"));
    let err = step
        .snapshot(&c)
        .expect_err("a hard stop was expected on a pre-existing DB");
    let msg = err.to_string();

    assert!(
        msg.contains("invok rollback"),
        "the message must name the real command, not a promise: {msg}"
    );
    assert!(
        !msg.contains("in arrivo"),
        "no more 'coming soon': the command exists: {msg}"
    );
    assert!(
        msg.contains("dropdb"),
        "the manual alternative stays too: {msg}"
    );
}

/// A-V3-6: the flag was renamed to what it does, but the historical name stays
/// accepted — it lives in customers' scripts and `.env` files, and breaking
/// those over a name would be worse than the defect.
#[test]
fn the_https_port_flag_keeps_its_historical_alias() {
    let nuovo = Cli::parse_from(["invok", "--with-nginx", "--open-https-port"]);
    assert!(nuovo.open_https_port);

    // the fallible parse, not the exiting one: if the alias vanished the test
    // would die without saying why — a guard that fails without explaining
    // itself forces the next reader to investigate from scratch.
    let storico = Cli::try_parse_from(["invok", "--with-nginx", "--enable-ssl"])
        .expect("`--enable-ssl` must stay accepted: it is the name the flag was born with, and it lives in customers' scripts");
    assert!(storico.open_https_port);

    let assente = Cli::parse_from(["invok"]);
    assert!(!assente.open_https_port);
}

/// A-V3-10: the progress bar must advance **even** on steps that cannot be
/// undone.
///
/// the two branches that give up — unknown step, unreadable snapshot —
/// signalled the start and not the end, so progress froze in the degraded
/// scenario, the only one where the user watches it. a rollback in progress
/// looked stuck.
#[test]
fn the_progress_advances_even_on_steps_that_cannot_be_undone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir.path().join("state.json");
    let model = SystemModel::new(fresh_state());

    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(&ctx(state_path.clone())));
    // a step this binary does not know…
    state.record(StepRecord {
        name: "step-from-a-future-version".to_string(),
        snapshot: serde_json::Value::Null,
    });
    // …and one whose snapshot is unreadable.
    state.record(StepRecord {
        name: "create-database".to_string(),
        snapshot: serde_json::json!({"non": "un PreState"}),
    });

    let (reporter, events) = common::RecordingReporter::new();
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let report = rollback::rollback_from_state(&state, &ctx(state_path), &make_ops, &reporter);

    let seen = common::events_of(&events);
    for step in ["step-from-a-future-version", "create-database"] {
        assert!(
            seen.contains(&format!("undo:{step}")),
            "the start must be signalled: {seen:?}"
        );
        assert!(
            seen.contains(&format!("undo-done:{step}")),
            "a step that cannot be undone is still an examined step: the bar must advance \
             (A-V3-10): {events:?}"
        );
    }

    // the report stays honest: neither was undone.
    assert!(!report.is_clean(), "two steps left undone must be reported");
}

// --- A-MD-2: the report checks the PROMISE, not only the outcomes -----------
//
// these three use the mock rather than the model, for a detail found while
// writing them: one step's undo queries the **real** filesystem instead of
// going through `SystemOps`. under the model it would see the machine running
// the tests, not the modelled state, and the test would prove something other
// than it believes.

/// **the defect, observed in the field.** the rollback declared "no leftovers:
/// the system is back to its previous state" while the perimeter directory was
/// still there.
///
/// no undo had failed, nor could it: faced with a **non-empty** directory the
/// step gives up — correctly, never a blind removal of other people's things —
/// and returns success. the verdict on outcomes was true; the promise the user
/// reads was not.
///
/// R7's lesson reversed: there a CI test asserted the leftover as expected,
/// here the report calls clean what is not. in both cases the assertion to
/// write first is the one about the **promise**, not the **mechanism**.
#[test]
fn a_surviving_home_is_reported_even_when_every_undo_succeeded() {
    let report = rollback_con_home(true);

    assert!(
        report.is_clean(),
        "no undo failed: on outcomes the rollback is clean"
    );
    assert_eq!(
        report.home_left_behind.as_deref(),
        Some(std::path::Path::new(HOME)),
        "…but the home is still there, and the report must say so: that is the \
         promise the user reads, not the count of undos"
    );
    assert!(
        report.has_anything_to_report(),
        "there is something to report, even though technically nothing failed"
    );
}

/// when the directory really goes, nothing is reported: a guaranteed alarm
/// teaches people to ignore alarms.
#[test]
fn a_home_that_is_gone_is_not_reported() {
    let report = rollback_con_home(false);

    assert_eq!(
        report.home_left_behind, None,
        "the home was removed: there is nothing to report"
    );
    assert!(!report.has_anything_to_report());
}

/// in a dry run the directory is there **by construction**, so reporting it
/// would be an alarm permanently on.
#[test]
fn the_dry_run_does_not_cry_wolf_about_the_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ctx = ctx(dir.path().join("state.json"));
    ctx.dry_run = true;

    let report = rollback_su_mock(&ctx, true);

    assert_eq!(report.home_left_behind, None);
}

/// runs a minimal rollback on a mock that declares the directory present or
/// absent.
fn rollback_con_home(home_esiste: bool) -> rollback::RollbackReport {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx(dir.path().join("state.json"));
    rollback_su_mock(&ctx, home_esiste)
}

fn rollback_su_mock(ctx: &invok::context::Context, home_esiste: bool) -> rollback::RollbackReport {
    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(ctx));
    state.record(StepRecord {
        name: "setup-log-dir".to_string(),
        snapshot: serde_json::to_value(invok::state::PreState::Untracked).expect("serialize"),
    });

    let make_ops = || -> Box<dyn SystemOps> {
        Box::new(
            common::MockSystemOps::new(common::MockConfig {
                path_exists: home_esiste,
                ..common::MockConfig::default()
            })
            .0,
        )
    };
    rollback::rollback_from_state(&state, ctx, &make_ops, &NoopReporter)
}

// --- A-V3-16: the installer can say who it is -------------------------------

/// the short and long installer-version flags print the **installer's** version
/// and exit, leaving `--version` — Odoo's version — alone.
///
/// found by installing a real package on a real machine: asking for the version
/// answered *"a value is required"*, and there was no way to ask the binary its
/// own. the temptation to rename the flag is the one R12 discarded: keep what
/// is in the field and add the missing way.
///
/// the fallible parse, since a version action exits the process.
#[test]
fn the_installer_can_be_asked_its_own_version() {
    use clap::error::ErrorKind;

    for flag in ["-V", "--installer-version"] {
        let err = Cli::try_parse_from(["invok", flag])
            .expect_err("a Version action stops the parsing: that is its job");
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayVersion,
            "`{flag}` must print the version, not a usage error"
        );
        assert!(
            err.to_string().contains(invok::INSTALLER_VERSION),
            "`{flag}` must print THIS binary's version: {err}"
        );
    }
}

/// and the plain version flag still means **Odoo's** version.
///
/// the half that makes the fix non-destructive: scripts and CI jobs in the
/// field pass it, and a change of meaning would silently install the wrong
/// release — silently, because they would still install *something*.
#[test]
fn the_odoo_version_flag_is_untouched() {
    let cli = Cli::try_parse_from(["invok", "--version", "18"])
        .expect("--version <VER> stays Odoo's version");
    assert_eq!(cli.version.as_deref(), Some("18"));
}

/// the manifest records who wrote it, and an old one stays readable.
#[test]
fn the_manifest_records_which_installer_wrote_it() {
    let config = config_fixture();
    assert_eq!(
        config.installer_version.as_deref(),
        Some(invok::INSTALLER_VERSION),
        "a manifest written now must say by whom"
    );

    // a manifest without the field reads as "I do not know". the field is
    // removed from the JSON as an object, not by string replacement: that
    // depends on serialisation order, and an order-dependent fixture lies the
    // day the order changes — on the first attempt the field was not removed at
    // all and the assertion passed for the wrong reason.
    let mut json: serde_json::Value = serde_json::to_value(&config).expect("serializza");
    json.as_object_mut()
        .expect("un oggetto")
        .remove("installer_version")
        .expect("the field was there");
    let vecchio: invok::state::InstallConfig =
        serde_json::from_value(json).expect("a pre-A-V3-16 manifest must stay readable");
    assert_eq!(
        vecchio.installer_version, None,
        "absent is not wrong: it is \"I do not know\", and nothing follows from that"
    );
}

/// the mismatch note speaks **only** when there is one, and names both
/// versions.
#[test]
fn a_manifest_from_another_installer_is_announced_not_refused() {
    use invok::state::version_mismatch_note;

    assert_eq!(
        version_mismatch_note(Some("2.3.0"), "2.3.0"),
        None,
        "the same version: there is nothing to say"
    );
    assert_eq!(
        version_mismatch_note(None, "2.3.0"),
        None,
        "an old manifest: \"I do not know\" is not a mismatch"
    );

    let note =
        version_mismatch_note(Some("2.3.0"), "2.1.0").expect("different versions: it must be said");
    assert!(
        note.contains("2.3.0") && note.contains("2.1.0"),
        "the note must name BOTH versions, or there is no telling what to do: {note}"
    );
    assert!(
        note.contains("unknown"),
        "and it must connect to the symptom it explains — the steps this binary does not know: {note}"
    );
}
