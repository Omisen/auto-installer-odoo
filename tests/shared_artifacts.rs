//! phase I2: the artifacts more than one instance depends on.
//!
//! `/opt/odoo`, the system packages, the PostgreSQL cluster, wkhtmltopdf, the
//! nginx installation, the firewall rule, the SELinux boolean: whichever
//! instance created them owns them, and its undo removes them — correctly,
//! while it is alone. with somebody else still installed, that same undo takes
//! the ground out from under a **running** instance.
//!
//! this is the anti-drop rule one level up, and the tests are written the same
//! way: what must survive matters more than what must go.
//!
//! everything runs against the model: no real command, no binary executed.

mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::model::{ModelState, SystemModel};
use invok::context::Context;
use invok::engine::Installer;
use invok::manifests::{discover_in, manifest_path_in, InstanceId};
use invok::progress::NoopReporter;
use invok::rollback::{self, RollbackReport, UndoOutcome};
use invok::secret::Secret;
use invok::state::{InstallConfig, InstallState, StepRecord};
use invok::step::Step;
use invok::steps::{self, ArtifactScope};
use invok::system_ops::SystemOps;

const HOME: &str = "/opt/odoo";
const SUDO_HOME: &str = "/home/alice";
const BASHRC: &str = "/home/alice/.bashrc";

/// a chain the factory can rebuild, holding both kinds of artifact: three that
/// are shared with every instance and three that belong to this one alone.
const CHAIN: &[&str] = &[
    "create-odoo-user",
    "bootstrap-prerequisites",
    "install-system-dependencies",
    "setup-postgres",
    "create-db-role",
    "create-database",
    "setup-systemd",
];

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn fresh_state() -> ModelState {
    let mut contents = HashMap::new();
    contents.insert(PathBuf::from(BASHRC), String::new());
    ModelState {
        paths: [HOME, SUDO_HOME, BASHRC]
            .iter()
            .map(PathBuf::from)
            .collect(),
        file_contents: contents,
        packages: set(&["coreutils"]),
        sudo_home: Some(SUDO_HOME.to_string()),
        ..Default::default()
    }
}

fn ctx_for(instance: Option<&str>, state_path: PathBuf) -> Context {
    let name = invok::instance::qualified_name(instance);
    let base = invok::instance::artifact_base(instance, "18");
    Context {
        instance: instance.map(str::to_string),
        odoo_user: name.clone(),
        db_user: name.clone(),
        db_name: name,
        db_password: Secret::new("pg-segreto"),
        admin_passwd: Secret::new("admin-segreto"),
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(HOME).join(base),
        port: 8069,
        sudo_user: Some("alice".to_string()),
        state_path,
        aggressive_rollback: true,
        ..Default::default()
    }
}

fn chain(model: &SystemModel, names: &[&str]) -> Vec<Box<dyn Step>> {
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    names
        .iter()
        .map(|name| {
            steps::step_by_name(name, &make_ops)
                .unwrap_or_else(|| panic!("the factory must know '{name}'"))
        })
        .collect()
}

/// installs `names` against the model and returns the manifest it left.
fn install(model: &SystemModel, names: &[&str], ctx: &Context) -> InstallState {
    let mut steps = chain(model, names);
    let mut installer = Installer::new();
    installer
        .execute(&mut steps, ctx)
        .expect("the chain must reach the end");
    InstallState::load(&ctx.state_path).expect("the manifest must have been written")
}

/// rolls the manifest back, telling it which other instances are installed.
fn rollback_with(
    model: &SystemModel,
    state: &InstallState,
    ctx: &Context,
    others: &[&str],
) -> RollbackReport {
    let others: Vec<String> = others.iter().map(|s| s.to_string()).collect();
    let mut ctx = ctx.clone();
    ctx.shared_in_use = !others.is_empty();
    let make_ops = || -> Box<dyn SystemOps> { model.boxed() };
    rollback::rollback_from_state_sharing_with(state, &ctx, &make_ops, &NoopReporter, &others)
}

fn outcome_of<'a>(report: &'a RollbackReport, step: &str) -> &'a UndoOutcome {
    &report
        .outcomes
        .iter()
        .find(|o| o.name == step)
        .unwrap_or_else(|| panic!("no outcome for '{step}'"))
        .outcome
}

// --- 1. the classification is total -----------------------------------------

/// every step in the canonical sequence must have a scope chosen **on purpose**.
///
/// the frozen table is the point: `artifact_scope` ends with a catch-all that
/// answers `Shared`, which is the safe reading of a name nobody classified — but
/// a *new* step inheriting it silently would be a step whose undo never runs on
/// a multi-instance machine, and nothing would say so. this test makes adding a
/// step a decision, exactly as `tests/apt_packages.rs` freezes the dependency
/// list.
#[test]
fn every_step_in_the_sequence_has_a_scope_chosen_on_purpose() {
    // (step, scope for the unnamed instance, scope for a named one)
    const EXPECTED: &[(&str, ArtifactScope, ArtifactScope)] = &[
        // the unnamed instance's home IS the shared root; a named one has its
        // own directory underneath, so half of that undo is its business.
        (
            "prepare-opt-root",
            ArtifactScope::Shared,
            ArtifactScope::Mixed,
        ),
        // `odoo` owns /opt/odoo itself; a named instance's user owns only its
        // own tree.
        (
            "create-odoo-user",
            ArtifactScope::Shared,
            ArtifactScope::OwnInstance,
        ),
        (
            "setup-log-dir",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "setup-cache-dir",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "bootstrap-prerequisites",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "install-system-dependencies",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "install-wkhtmltopdf",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "setup-postgres",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "create-db-role",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "create-database",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "clone-odoo-repo",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "create-virtualenv",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "install-python-requirements",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "generate-config",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "setup-data-dir",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "initialize-odoo-database",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "setup-systemd",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        // nginx itself is shared; the realigning reload inside the same undo is
        // not, which is why it is Mixed and not Shared.
        ("nginx-install", ArtifactScope::Mixed, ArtifactScope::Mixed),
        (
            "nginx-write-config",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "nginx-enable-site",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "nginx-selinux",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "nginx-firewall",
            ArtifactScope::Shared,
            ArtifactScope::Shared,
        ),
        (
            "nginx-reload",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "write-control-script",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
        (
            "patch-bashrc",
            ArtifactScope::OwnInstance,
            ArtifactScope::OwnInstance,
        ),
    ];

    let make_ops = invok::system_ops::backend_factory(Default::default())
        .expect("the Debian family has a backend");
    let canonical = steps::canonical_step_names(&make_ops);

    let classified: HashSet<&str> = EXPECTED.iter().map(|(name, _, _)| *name).collect();
    let sequence: HashSet<&str> = canonical.iter().map(|s| s.as_str()).collect();

    let unclassified: Vec<&&str> = sequence.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "these steps have no scope in this table, so they would silently inherit \
         `Shared` and never be undone on a machine with more than one instance: {unclassified:?}"
    );
    let vanished: Vec<&&str> = classified.difference(&sequence).collect();
    assert!(
        vanished.is_empty(),
        "these steps are classified but no longer in the sequence: {vanished:?}"
    );

    for (name, unnamed, named) in EXPECTED {
        assert_eq!(
            steps::artifact_scope(name, true),
            *unnamed,
            "scope of '{name}' for the unnamed instance"
        );
        assert_eq!(
            steps::artifact_scope(name, false),
            *named,
            "scope of '{name}' for a named instance"
        );
    }
}

// --- 2. the rule, end to end against the model ------------------------------

/// **the test of the phase**: with another instance installed, what they share
/// survives while this instance's own artifacts go.
#[test]
fn with_another_instance_installed_the_shared_artifacts_survive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let ctx = ctx_for(Some("cliente-x"), dir.path().join("state.json"));

    let state = install(&model, CHAIN, &ctx);
    let installed = model.snapshot();
    assert!(installed.pg_dbs.contains("odoo-cliente-x"));
    assert!(installed.svc_enabled.contains("postgresql"));

    let report = rollback_with(&model, &state, &ctx, &["default"]);
    let after = model.snapshot();

    // its own artifacts are gone.
    assert!(
        !after.pg_dbs.contains("odoo-cliente-x"),
        "the instance's database is its own and must be dropped"
    );
    assert!(
        !after.pg_roles.contains("odoo-cliente-x"),
        "and so is its role"
    );
    assert!(
        !after.users.contains("odoo-cliente-x"),
        "a named instance's system user belongs to it alone"
    );

    // what the machine shares does not move.
    assert!(
        after.svc_enabled.contains("postgresql"),
        "PostgreSQL serves the other instance too: stopping it would take its database away"
    );
    assert!(
        installed
            .packages
            .iter()
            .all(|p| after.packages.contains(p)),
        "not one package may be purged: the other instance is running on them"
    );
    assert!(
        after.paths.contains(&PathBuf::from(HOME)),
        "/opt/odoo is where every instance lives"
    );

    // and the report says so, naming who is still using them.
    assert_eq!(
        outcome_of(&report, "setup-postgres"),
        &UndoOutcome::LeftShared(vec!["default".to_string()])
    );
    assert_eq!(
        outcome_of(&report, "install-system-dependencies"),
        &UndoOutcome::LeftShared(vec!["default".to_string()])
    );
    assert_eq!(
        outcome_of(&report, "create-database"),
        &UndoOutcome::Undone,
        "its own artifacts are undone as they always were"
    );
    assert!(
        !report.is_clean(),
        "something this manifest describes is still on the machine, so the manifest is kept"
    );
}

/// the control, and it is not a formality: without it the test above would pass
/// on an installer that never undoes anything at all.
#[test]
fn alone_on_the_machine_nothing_is_held_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let ctx = ctx_for(Some("cliente-x"), dir.path().join("state.json"));

    let state = install(&model, CHAIN, &ctx);
    let report = rollback_with(&model, &state, &ctx, &[]);
    let after = model.snapshot();

    assert!(
        !after.svc_enabled.contains("postgresql"),
        "with nobody else here, PostgreSQL is ours to stop"
    );
    assert_eq!(outcome_of(&report, "setup-postgres"), &UndoOutcome::Undone);
    assert!(
        report
            .outcomes
            .iter()
            .all(|o| o.outcome == UndoOutcome::Undone),
        "alone, every undo runs: {:?}",
        report.outcomes
    );
}

/// `nginx-install` is the other step that owns **both**, and the half that must
/// still happen is the least obvious one.
///
/// its undo ends with a **realigning reload**: our vhost is gone from disk, but
/// a running nginx goes on serving the config it loaded (A1.4, found by the e2e
/// in R3). skipping the whole undo because nginx is shared would leave the
/// customer's nginx serving a vhost whose files no longer exist — the same
/// defect the reload was put there to prevent.
#[test]
fn nginx_stays_installed_and_running_but_is_still_reloaded() {
    let dir = tempfile::tempdir().expect("tempdir");
    // nginx is seeded **running but not enabled**, which is what makes both
    // halves of the undo reachable in one test: the step still enables it (so
    // that half is ours to give back) and the final reload only happens on a
    // service that is actually up.
    let mut initial = fresh_state();
    initial.svc_active.insert("nginx".to_string());
    let model = SystemModel::new(initial);
    let ctx = {
        let mut c = ctx_for(Some("cliente-x"), dir.path().join("state.json"));
        c.with_nginx = true;
        c
    };

    let state = install(&model, &["nginx-install"], &ctx);
    let installed = model.snapshot();
    assert!(installed.packages.contains("nginx"));
    assert!(installed.svc_active.contains("nginx"));

    let report = rollback_with(&model, &state, &ctx, &["default"]);
    let after = model.snapshot();

    assert!(
        after.packages.contains("nginx"),
        "nginx serves the other instance too: purging it would take that instance offline"
    );
    assert!(
        after.svc_active.contains("nginx") && after.svc_enabled.contains("nginx"),
        "and it must be left running, for the same reason"
    );
    assert_eq!(
        after.nginx_loaded_sites,
        Some(std::collections::HashSet::new()),
        "but the reload still happened: what nginx serves must match what is on disk, or \
         it goes on serving a vhost whose files are gone"
    );
    assert_eq!(
        outcome_of(&report, "nginx-install"),
        &UndoOutcome::LeftShared(vec!["default".to_string()])
    );
}

// --- 3. the tombstone -------------------------------------------------------

/// what a manifest looks like after that rollback, and why it is not an
/// instance any more.
///
/// counting it as one would deadlock the machine: two half-removed
/// installations, each protecting the other's shared artifacts, neither ever
/// able to finish.
#[test]
fn a_manifest_left_with_only_shared_records_is_a_tombstone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let live = InstallState {
        completed: vec![
            StepRecord {
                name: "prepare-opt-root".to_string(),
                snapshot: serde_json::json!({"shared_root": "CreatedByUs"}),
            },
            StepRecord {
                name: "create-database".to_string(),
                snapshot: serde_json::json!("CreatedByUs"),
            },
        ],
        config: Some(config_named("cliente-x")),
        finished: true,
    };
    let tomb = InstallState {
        completed: vec![StepRecord {
            name: "prepare-opt-root".to_string(),
            snapshot: serde_json::json!({"shared_root": "CreatedByUs"}),
        }],
        config: Some(config_named("cliente-y")),
        finished: false,
    };
    write(&manifest_path_in(dir.path(), Some("cliente-x")), &live);
    write(&manifest_path_in(dir.path(), Some("cliente-y")), &tomb);

    let d = discover_in(dir.path(), &manifest_path_in(dir.path(), None), &[]);
    let by = |name: &str| {
        d.found
            .iter()
            .find(|f| f.id == InstanceId::Named(name.to_string()))
            .expect("found")
    };

    assert!(by("cliente-x").is_live());
    assert!(
        !by("cliente-y").is_live(),
        "only shared records left: the instance is gone, the artifacts it owns for \
         everybody are not"
    );
    assert!(
        by("cliente-y").owns_anything(),
        "and it is not empty either: that record is the only trace of who owns them"
    );

    // which is what the two selectors `--all` walks are built on.
    assert_eq!(
        d.live_others(&InstanceId::Named("cliente-y".to_string())),
        vec!["cliente-x"],
        "a tombstone does not stop the live instance from being seen"
    );
    assert!(
        d.live_others(&InstanceId::Named("cliente-x".to_string()))
            .is_empty(),
        "and it does not hold the shared artifacts hostage either"
    );
    assert_eq!(
        d.tombstones()
            .iter()
            .map(|f| f.id.to_string())
            .collect::<Vec<_>>(),
        vec!["cliente-y"],
        "the second pass of --all comes back for exactly these"
    );
}

fn config_named(name: &str) -> InstallConfig {
    InstallConfig {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        instance: Some(name.to_string()),
        odoo_user: format!("odoo-{name}"),
        db_user: format!("odoo-{name}"),
        db_name: format!("odoo-{name}"),
        odoo_home: PathBuf::from(HOME),
        install_dir: PathBuf::from(HOME).join(format!("odoo-{name}")),
        port: 8069,
        odoo_logfile: None,
        with_nginx: false,
        sudo_user: Some("alice".to_string()),
        os_family: Default::default(),
        installer_version: Some(invok::INSTALLER_VERSION.to_string()),
    }
}

fn write(path: &std::path::Path, state: &InstallState) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, serde_json::to_vec_pretty(state).expect("serialise")).expect("write");
}

/// `A-V6-11`: with other instances installed, the shared root still being there
/// is **the rule working**, not a residue to warn about.
///
/// found in the field, not by a test: the report printed the shared-artifacts
/// section — eight steps left in place — and then, eight lines below, "it holds
/// something we did not create" (it holds the *other instance*, which we
/// created) and "everything the installer had created has been removed". Two
/// sentences that were plainly false, contradicting the section above them.
///
/// the check exists for A-MD-2's question — *did the promise hold?* — and that
/// question only has meaning when this instance was the last one.
#[test]
fn the_home_is_not_reported_as_a_residue_while_another_instance_lives_there() {
    let dir = tempfile::tempdir().expect("tempdir");
    let model = SystemModel::new(fresh_state());
    let ctx = ctx_for(Some("cliente-x"), dir.path().join("state.json"));
    let state = install(&model, CHAIN, &ctx);

    let with_others = rollback_with(&model, &state, &ctx, &["default"]);
    assert!(
        with_others.home_left_behind.is_none(),
        "the shared root was kept on purpose: reporting it as a leftover contradicts the \
         'left in place' section printed just above it"
    );

    // and the check must still fire when this instance IS the last one — that
    // is the case A-MD-2 exists for.
    let alone = rollback_with(&model, &state, &ctx, &[]);
    assert_eq!(
        alone.home_left_behind,
        Some(PathBuf::from(HOME)),
        "alone on the machine, a home that survives every undo is exactly what must be reported"
    );
}
