//! the `snapshot_value` ⇄ `rehydrate` symmetry (R4): the property that makes
//! rollback from disk trustworthy.
//!
//! a rollback from persisted state rebuilds each step and puts the original
//! snapshot back. losing even one field there makes the undo decide wrongly,
//! and the worst wrong decision is dropping a database the snapshot marked
//! `Preexisting`. so this does not prove "the rollback works": it proves
//! **rehydrated ≡ original**, step by step and behaviourally.
//!
//! three levels: **JSON identity**, where the value after rehydration is
//! identical for every step; **undo equivalence**, where the same chain undone
//! live and rehydrated reaches the same final state; and a **field fixture**,
//! the state file really observed on a VM after a Ctrl-C.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use invok::context::Context;
use invok::secret::Secret;
use invok::state::{InstallState, PreState};
use invok::step::Step;
use invok::steps;
use invok::system_ops::SystemOps;

const HOME: &str = "/opt/odoo";
const INSTALL: &str = "/opt/odoo/odoo18";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";
const BASHRC_ORIG: &str = "alias ll='ls -la'\nexport EDITOR=vim\n";

/// the real chain rebuildable from the factory, with mockable `SystemOps`.
///
/// three steps are excluded: one uses the filesystem directly, one writes a
/// real temporary, and one would really download. their cycles have dedicated
/// tests.
const CHAIN: &[&str] = &[
    "create-odoo-user",
    "setup-log-dir",
    "setup-cache-dir",
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

/// builds the chain from the factory, binding every step to one model.
fn chain_from_factory(model: &SystemModel, names: &[&str]) -> Vec<Box<dyn Step>> {
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    names
        .iter()
        .map(|name| {
            steps::step_by_name(name, &make_ops)
                .unwrap_or_else(|| panic!("the factory must know step '{name}'"))
        })
        .collect()
}

/// runs the chain to the end and returns the JSON snapshots.
fn run_chain(steps: &mut [Box<dyn Step>], ctx: &Context) -> Vec<serde_json::Value> {
    for step in steps.iter_mut() {
        let name = step.name().to_string();
        step.snapshot(ctx)
            .unwrap_or_else(|e| panic!("snapshot of '{name}' failed: {e}"));
        step.run(ctx)
            .unwrap_or_else(|e| panic!("run of '{name}' failed: {e}"));
    }
    steps.iter().map(|s| s.snapshot_value()).collect()
}

// --- 1. JSON identity, step by step -----------------------------------------

#[test]
fn every_step_rehydrates_to_an_identical_snapshot() {
    // the finest level: for **every** step, the JSON produced after rehydration
    // must be identical to the original. a forgotten field shows up at once,
    // named by the step that lost it.
    let model = SystemModel::new(fresh_state());
    let ctx = ctx();
    let mut live = chain_from_factory(&model, CHAIN);
    let snapshots = run_chain(&mut live, &ctx);

    let clean = SystemModel::new(fresh_state());
    for (step, original) in live.iter().zip(snapshots.iter()) {
        let name = step.name();
        let make_ops = || -> Box<dyn SystemOps> { clean.boxed() };
        let mut fresh = steps::step_by_name(name, &make_ops)
            .unwrap_or_else(|| panic!("factory without '{name}'"));

        fresh
            .rehydrate(original)
            .unwrap_or_else(|e| panic!("rehydrate of '{name}' failed: {e}"));

        assert_eq!(
            &fresh.snapshot_value(),
            original,
            "'{name}': snapshot_value after rehydrate must be identical to the original"
        );
    }
}

#[test]
fn a_corrupt_snapshot_fails_rehydration_instead_of_defaulting() {
    // fail-closed: an unreadable snapshot must NOT yield a step with default
    // state. that would be harmless only by accident, and for the steps that
    // restore a customer's backup it would mean not restoring it. an error is
    // better, and the rollback reports it as a leftover.
    let model = SystemModel::new(fresh_state());
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };

    for name in ["create-database", "setup-postgres", "patch-bashrc"] {
        let mut step = steps::step_by_name(name, &make_ops).expect("factory");
        let bogus = serde_json::json!({ "questo": "not a valid snapshot" });
        assert!(
            step.rehydrate(&bogus).is_err(),
            "'{name}': a snapshot that cannot be deserialised must be an error"
        );
    }
}

// --- 2. behavioural equivalence of the undo ---------------------------------

#[test]
fn rehydrated_steps_undo_exactly_like_the_live_ones() {
    // the property that really matters: not "the JSON matches" but "the system
    // ends up in the same place". two identical models, one chain undone two
    // ways, must converge.
    let ctx = ctx();

    // (a) in-process: the steps that ran are the ones that undo.
    let live_model = SystemModel::new(fresh_state());
    let mut live = chain_from_factory(&live_model, CHAIN);
    let snapshots = run_chain(&mut live, &ctx);
    let after_run = live_model.snapshot();
    for step in live.iter().rev() {
        let _ = step.undo(&ctx);
    }
    let undone_in_process = live_model.snapshot();

    // (b) from disk: the same post-run state, with fresh rehydrated steps.
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
        "the undo from rehydrated steps must reach the same state as the in-process undo"
    );
    assert_eq!(
        undone_from_disk,
        fresh_state(),
        "and that state is the virgin system"
    );
}

#[test]
fn rehydration_without_the_snapshot_would_undo_the_wrong_things() {
    // the counter-proof, mutation testing in the form of a test: undoing with
    // pristine steps would be silently inert — every `PreState` `Untracked` and
    // every undo a no-op. the reason `rehydrate` exists, made explicit.
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
        "without rehydrate the rollback cannot work: if this assertion falls, the undos \
         are acting without consulting the PreState"
    );
    assert!(
        disk_model.snapshot().users.contains("odoo"),
        "without the rehydrated PreState the user we created is not removed"
    );
}

// --- 3. a field fixture: the state file observed on a VM --------------------

/// the **real** state file read on a VM after a Ctrl-C mid-installation.
///
/// exactly what the `rollback` command consumes, so rehydration is proven
/// against the true format rather than the one the tests produce.
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
    serde_json::from_str(FIELD_STATE).expect("the state file from the field must stay readable")
}

#[test]
fn the_real_state_file_still_parses_and_has_no_config() {
    // format compatibility: a pre-R4 file stays readable, and a missing config
    // is detectable — which is what `rollback` stops on rather than guessing
    // artifact names.
    let state = field_state();
    assert_eq!(state.completed.len(), 4);
    assert_eq!(state.completed[0].name, "prepare-opt-root");
    assert!(
        state.config.is_none(),
        "pre-R4 state files carry no configuration: the rollback command must be able to \
         tell"
    );
}

/// rehydrates one step from the field fixture and undoes it on the model.
fn undo_from_field_state(model: &SystemModel, step_name: &str, ctx: &Context) {
    let state = field_state();
    let record = state
        .completed
        .iter()
        .find(|r| r.name == step_name)
        .unwrap_or_else(|| panic!("the fixture must contain '{step_name}'"));
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    let mut step = steps::step_by_name(step_name, &make_ops).expect("factory");
    step.rehydrate(&record.snapshot)
        .unwrap_or_else(|e| panic!("rehydrate of '{step_name}' from the fixture: {e}"));
    step.undo(ctx)
        .unwrap_or_else(|e| panic!("undo of '{step_name}': {e}"));
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
        "user_prestate=CreatedByUs in the fixture: the user must be removed"
    );
    assert!(
        !s.groups.contains("odoo"),
        "and its dedicated group with it"
    );
    assert!(
        s.paths.contains(&PathBuf::from(HOME)),
        "the home is NOT this step's: userdel without -r, PrepareOptRoot removes it"
    );
}

#[test]
fn field_state_apt_delta_purges_only_the_delta() {
    let mut init = fresh_state();
    // the system as it would be mid-installation: pre-existing plus our delta.
    for pkg in ["git", "curl", "python3-pip", "build-essential", "libpq-dev"] {
        init.packages.insert(pkg.to_string());
    }
    let model = SystemModel::new(init);

    undo_from_field_state(&model, "install-system-dependencies", &ctx());

    let s = model.snapshot();
    for pkg in ["python3-pip", "build-essential", "libpq-dev"] {
        assert!(
            !s.packages.contains(pkg),
            "'{pkg}' is in the delta: it must be purged"
        );
    }
    for pkg in ["git", "curl"] {
        assert!(
            s.packages.contains(pkg),
            "'{pkg}' was already installed before us: it must NOT be touched"
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

    // not aggressive: stop and disable, but no purge (D3).
    let mut c = ctx();
    c.aggressive_rollback = false;
    undo_from_field_state(&model, "setup-postgres", &c);

    let s = model.snapshot();
    assert!(
        !s.svc_active.contains("postgresql"),
        "the 'active' axis is CreatedByUs: the service must be stopped"
    );
    assert!(
        !s.svc_enabled.contains("postgresql"),
        "the 'enabled' axis is CreatedByUs: the service must be disabled"
    );
    assert!(
        s.packages.contains("postgresql"),
        "the 'installed' axis is CreatedByUs, but without --aggressive-rollback there is \
         no purge: stop and disable are reversible, a purge is not (D3)"
    );
}

#[test]
fn field_state_preexisting_prepare_opt_root_is_a_noop() {
    // the bare-string form the `PreState` really serialises to. rehydrated, it
    // must make the undo inert.
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
        "/opt/odoo was Preexisting: the undo must not remove it"
    );
}
