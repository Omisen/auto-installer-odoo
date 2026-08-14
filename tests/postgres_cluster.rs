//! M3: initialising the PostgreSQL cluster, `setup-postgres`'s fourth axis.
//!
//! the heaviest divergence between the families. on one the package's
//! post-install creates the cluster and the service starts, so there was never
//! an init step because none was needed. on the other the server package
//! **initialises nothing**: without an explicit init the service does not
//! start, and the step failed at its final check with a message sending the
//! reader to the journal instead of naming the cause.
//!
//! it is not "one more command": the init **produces an artifact**, the data
//! directory, which without a `PreState` of its own would appear unrecorded —
//! that is, not undoable (A-R5-3).

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::OsFamily;
use invok::error::StepError;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::setup_postgres::{PostgresSnapshot, SetupPostgres};

fn ctx(aggressive: bool) -> Context {
    Context {
        dry_run: false,
        aggressive_rollback: aggressive,
        db_name: "odoo".to_string(),
        ..Default::default()
    }
}

fn esegui(cfg: MockConfig) -> (SetupPostgres, common::OpLog) {
    let (ops, log) = MockSystemOps::new(cfg);
    let mut step = SetupPostgres::with_ops(Box::new(ops));
    step.snapshot(&ctx(false)).expect("snapshot");
    step.run(&ctx(false)).expect("run");
    (step, log)
}

fn snap(step: &SetupPostgres) -> PostgresSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

// --- the cluster is initialised only where needed ---------------------------

/// where the cluster must be created, the operation is **recorded**.
#[test]
fn on_fedora_the_cluster_is_initialized_and_recorded() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });

    assert!(
        ops_of(&log).contains(&Op::InitPostgresCluster),
        "without the init the service does not start: {:?}",
        ops_of(&log)
    );
    assert_eq!(
        snap(&step).cluster_initialized,
        PreState::CreatedByUs,
        "we created it: it is our artifact, and the undo must know"
    );
}

/// where the package does it itself, **nothing** is initialised: an extra init
/// on an existing cluster is not harmless.
#[test]
fn on_debian_nothing_is_initialized() {
    let (step, log) = esegui(MockConfig::default());

    assert!(
        !ops_of(&log).contains(&Op::InitPostgresCluster),
        "on this family the package's post-install creates the cluster"
    );
    assert_eq!(
        snap(&step).cluster_initialized,
        PreState::Untracked,
        "nothing to initialise means nothing to undo"
    );
}

/// an **already-initialised** cluster is not touched: it is `Preexisting`, and
/// reinitialising it would destroy the databases it holds.
#[test]
fn an_existing_cluster_is_left_alone() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_cluster_initialized: true,
        ..MockConfig::default()
    });

    assert!(
        !ops_of(&log).contains(&Op::InitPostgresCluster),
        "the cluster was already there: an init on a populated data directory is exactly \
         what must not happen"
    );
    assert_eq!(snap(&step).cluster_initialized, PreState::Preexisting);
}

/// the init happens **before** enable and start.
///
/// the order is not cosmetic: it is the whole reason this is an axis of
/// `setup-postgres` and not a step of its own. starting before initialising
/// fails, and the step would stop at its final check pointing at the symptom
/// instead of the cause.
#[test]
fn the_cluster_is_initialized_before_the_service_starts() {
    let (_step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });

    let ops = ops_of(&log);
    let posizione = |cerca: fn(&Op) -> bool| ops.iter().position(cerca);

    let init = posizione(|op| matches!(op, Op::InitPostgresCluster)).expect("init eseguito");
    let install = posizione(|op| matches!(op, Op::PkgInstall(_))).expect("install eseguito");
    let start = posizione(|op| matches!(op, Op::ServiceStart(_))).expect("start eseguito");

    assert!(
        install < init,
        "the package is installed first, then the cluster initialised: {ops:?}"
    );
    assert!(
        init < start,
        "the init must precede the start, or the service does not come up: {ops:?}"
    );
}

// --- the undo: the cluster follows the package's policy ---------------------

/// **without the aggressive flag the cluster stays.**
///
/// an empty data directory is an inert leftover; somebody else's data is not.
/// the same asymmetry as the package purge: stop and disable are reversible,
/// removing data is not.
#[test]
fn without_the_aggressive_flag_the_cluster_survives() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    step.undo(&ctx(false)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "the cluster is not removed without the explicit flag: {:?}",
        ops_of(&log)
    );
}

/// with the flag **and** no third-party database in the cluster, it is removed.
#[test]
fn with_the_aggressive_flag_and_an_empty_cluster_it_is_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        // only ours and the maintenance database: no third-party data.
        pg_databases_list: vec!["odoo".to_string(), "postgres".to_string()],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(p)
            if p.ends_with("data"))),
        "with --aggressive-rollback and no third-party database the cluster goes: {:?}",
        ops_of(&log)
    );
}

/// **the protection that matters.** with the flag but *other people's*
/// databases in the cluster, the data directory is left alone.
///
/// the existing question — "is there anything besides our database?" — protects
/// here exactly as it does for the package. a data directory holds **all** the
/// cluster's databases: removing it to undo our installation would be the
/// anti-drop violated on a larger scale.
#[test]
fn a_cluster_hosting_other_databases_is_never_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_databases_list: vec![
            "odoo".to_string(),
            "postgres".to_string(),
            "fatturazione_cliente".to_string(),
        ],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "the cluster holds a database we did not create: the data directory holds them \
         ALL, and removing it would destroy the customer's data"
    );
}

/// a **pre-existing** cluster is not removed even with the flag: it is not
/// ours.
#[test]
fn a_preexisting_cluster_is_never_removed() {
    let (step, log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        pg_cluster_initialized: true,
        pg_databases_list: vec!["postgres".to_string()],
        ..MockConfig::default()
    });
    step.undo(&ctx(true)).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::RemoveDirAll(_))),
        "PreState::Preexisting: it was there before us, not ours to destroy"
    );
}

// --- snapshot compatibility -------------------------------------------------

/// a snapshot written **before M3** lacks the field and reads as `Untracked`.
///
/// which is the truth for every existing installation: they are all on the
/// family where we never initialised the cluster. making it unreadable would
/// make the step undoable no more.
#[test]
fn a_snapshot_written_before_this_axis_still_rehydrates() {
    let legacy = serde_json::json!({
        "installed": "CreatedByUs",
        "enabled": "CreatedByUs",
        "active": "CreatedByUs"
    });

    let (ops, _log) = MockSystemOps::new(MockConfig::default());
    let mut step = SetupPostgres::with_ops(Box::new(ops));
    step.rehydrate(&legacy)
        .expect("a pre-M3 snapshot must stay readable");

    let s = snap(&step);
    assert_eq!(
        s.installed,
        PreState::CreatedByUs,
        "the known axes are read"
    );
    assert_eq!(
        s.cluster_initialized,
        PreState::Untracked,
        "without the field no cluster is ours: that is what those installations were"
    );
}

/// and the fourth axis survives the round trip, like the other three.
#[test]
fn the_fourth_axis_survives_serialisation() {
    let (step, _log) = esegui(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    let serializzato = step.snapshot_value();

    let (ops, _log2) = MockSystemOps::new(MockConfig::default());
    let mut riletto = SetupPostgres::with_ops(Box::new(ops));
    riletto.rehydrate(&serializzato).expect("rehydrate");

    assert_eq!(snap(&riletto).cluster_initialized, PreState::CreatedByUs);
}

// --- the real implementations, not the mock's -------------------------------

/// **the hole mutation testing found.**
///
/// the tests above go through the mock, which picks its *own* data directory
/// from the modelled family. fine for the step's logic — but the real
/// implementations were exercised by nothing, and a mutation making the wrong
/// family declare a directory to initialise survived the whole suite.
///
/// in the field that would have run an init command that does not exist there,
/// and a step failing after PostgreSQL was installed: a rollback for an
/// invented reason.
///
/// when the mock replicates a production decision, that decision must be
/// exercised **where it is written** too.
#[test]
fn each_family_declares_its_own_cluster_policy() {
    use invok::distro::{debian::Debian, fedora::Fedora, Distro};

    assert_eq!(
        Debian::new().postgres_data_dir(),
        None,
        "on that family the package's post-install creates and starts the cluster: \
         declaring a data directory here would run an init that is not needed, with a \
         command that does not exist there"
    );

    assert_eq!(
        Fedora::new().postgres_data_dir(),
        Some(std::path::PathBuf::from("/var/lib/pgsql/data")),
        "on this family the cluster must be created, at the distribution's default path"
    );
}

/// the init on a family that does not need one is a **succeeding no-op**, not
/// an error.
///
/// it matters because the step calls it only when a directory is declared: a
/// panic or an error here would be a branch that cannot run, and the day
/// somebody changed that condition the defect would land in the field instead
/// of here.
#[test]
fn initialising_where_it_is_not_needed_succeeds_quietly() {
    use invok::distro::{debian::Debian, Distro};

    assert!(
        Debian::new().init_postgres_cluster().is_ok(),
        "on this family there is nothing to do, and \"nothing to do\" is a success"
    );
}

// --- A-MD-6: the two answers to "where is the cluster" ----------------------

/// the unit's environment, read the way the setup tool reads it.
///
/// the fixture is the command's **real** format: one line with several
/// space-separated assignments. writing it from memory as one line per variable
/// would give a parser matching nothing — it has happened twice already
/// (A-R8-1-ter).
#[test]
fn the_declared_pgdata_is_read_the_way_postgresql_setup_reads_it() {
    use invok::distro::fedora::pgdata_from_environment;
    use std::path::PathBuf;

    assert_eq!(
        pgdata_from_environment("Environment=PGDATA=/var/lib/pgsql/data\n"),
        Some(PathBuf::from("/var/lib/pgsql/data"))
    );
    assert_eq!(
        pgdata_from_environment(
            "Environment=PG_OOM_ADJUST_FILE=/proc/self/oom_score_adj PGDATA=/srv/pg/data\n"
        ),
        Some(PathBuf::from("/srv/pg/data")),
        "the data directory is not necessarily the line's first variable"
    );
    assert_eq!(
        pgdata_from_environment("Environment=PGDATA=/var/lib/pgsql/data PGDATA=/srv/pg/data\n"),
        Some(PathBuf::from("/srv/pg/data")),
        "as in systemd, the last definition wins: that is how a drop-in acts"
    );
    assert_eq!(
        pgdata_from_environment("Environment=\n"),
        None,
        "a unit without the variable means \"I do not know\", not an empty path"
    );
    assert_eq!(
        pgdata_from_environment(""),
        None,
        "and an absent output does not translate into a path either"
    );
}

/// the rule: refuse **only** when the two answers genuinely diverge.
#[test]
fn a_conflict_is_only_a_conflict_when_both_answers_exist_and_differ() {
    use invok::steps::setup_postgres::cluster_path_conflict;
    use std::path::Path;

    let nostro = Path::new("/var/lib/pgsql/data");

    assert!(
        cluster_path_conflict(nostro, Some(Path::new("/var/lib/pgsql/data"))).is_none(),
        "the same path: the normal case on every machine of that family"
    );
    assert!(
        cluster_path_conflict(nostro, None).is_none(),
        "\"I do not know\" is not a conflict: blindness is not divergence"
    );

    let message = cluster_path_conflict(nostro, Some(Path::new("/srv/pg/data")))
        .expect("different paths: it must be refused");
    assert!(
        message.contains("/srv/pg/data") && message.contains("/var/lib/pgsql/data"),
        "the message must name BOTH paths, or there is no telling what does not add up: \
         {message}"
    );
    assert!(
        message.contains("aggressive-rollback"),
        "and it must say which damage is being avoided: {message}"
    );
}

/// the step stops **in the snapshot**, before any mutation.
///
/// the difference between a precondition and an undo: there is nothing to undo
/// because nothing was done. and the case is genuinely reachable — an existing
/// PostgreSQL with a drop-in moving the data directory — which is exactly the
/// machine where the damage would be worst.
#[test]
fn a_cluster_configured_elsewhere_stops_the_installation_before_it_starts() {
    let cfg = MockConfig {
        family: OsFamily::Fedora,
        pg_declared_data_dir: Some(std::path::PathBuf::from("/srv/pg/data")),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = SetupPostgres::with_ops(Box::new(mock));

    let err = step
        .snapshot(&ctx(false))
        .expect_err("a data directory other than ours must stop the step");
    assert!(
        matches!(err, StepError::Precondition(_)),
        "it must be a precondition, not just any error: {err:?}"
    );
    assert!(
        ops_of(&log).is_empty(),
        "the snapshot must have mutated nothing: {:?}",
        ops_of(&log)
    );
}

/// and when the answers agree — or the unit says nothing — the step proceeds as
/// it always has.
///
/// the half that makes the check a check: without it, "always refuse" would
/// pass the test above unnoticed.
#[test]
fn the_usual_fedora_is_not_stopped_by_this_check() {
    for declared in [
        None,
        Some(std::path::PathBuf::from(
            invok::distro::fedora::POSTGRES_DATA_DIR,
        )),
    ] {
        let cfg = MockConfig {
            family: OsFamily::Fedora,
            pg_declared_data_dir: declared.clone(),
            ..Default::default()
        };
        let (mock, _log) = MockSystemOps::new(cfg);
        let mut step = SetupPostgres::with_ops(Box::new(mock));
        assert!(
            step.snapshot(&ctx(false)).is_ok(),
            "with data directory {declared:?} the step must proceed"
        );
    }
}
