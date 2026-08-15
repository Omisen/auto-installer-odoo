//! phase I1: one manifest per instance, found as **one uniform list**.
//!
//! what is being defended:
//!
//! 1. **an installation made by any earlier version is still found.** the
//!    unnamed instance's manifest is not moved, and the historical paths R7 left
//!    readable keep being read. losing sight of a manifest is not an
//!    inconvenience — it is an instance nobody can uninstall without guessing
//!    the names of its artifacts.
//! 2. **discovery returns one shape.** the count of instances is what will
//!    decide, in I2, whether `/opt/odoo` and the system user come off. it must
//!    not be built on top of "two formats and two paths" (`A-V6-2`).
//! 3. **not knowing is never nothing.** an unreadable manifest is reported as a
//!    problem, not silently absent.

use std::fs;
use std::path::{Path, PathBuf};

use invok::config::{RawConfig, ResolvedConfig};
use invok::context::Context;
use invok::instance::{validate_instance, UNNAMED_ID};
use invok::manifests::{
    discover_in, manifest_path_in, port_conflict, select, Found, InstanceId, Selection,
};
use invok::state::{InstallConfig, InstallState, StepRecord};

// --- fixtures ---------------------------------------------------------------

/// a resolved installation, as the cascade would produce it.
fn config_of(instance: Option<&str>, port: u16) -> InstallConfig {
    let resolved = ResolvedConfig::resolve(
        &RawConfig {
            instance: instance.map(str::to_string),
            port: Some(port.to_string()),
            admin_passwd: Some("s3cret".to_string()),
            ..Default::default()
        },
        &RawConfig::default(),
        &RawConfig::default(),
        /* interactive */ false,
    )
    .expect("resolution");
    let ctx = Context::from_resolved(resolved, false, PathBuf::from("/tmp/unused.json"));
    InstallConfig::from_context(&ctx)
}

/// a manifest of a **live** instance: one step that owns something shared and
/// one that is entirely its own.
///
/// both matter. a manifest recording only shared steps is a *tombstone* — the
/// instance is gone and what is left is the record of what it owns on behalf of
/// the others — and `Found::is_live` tells them apart.
fn state_of(instance: Option<&str>, port: u16) -> InstallState {
    InstallState {
        completed: vec![
            StepRecord {
                name: "prepare-opt-root".to_string(),
                snapshot: serde_json::json!({"shared_root": "CreatedByUs", "instance_home": "Untracked"}),
            },
            StepRecord {
                name: "create-database".to_string(),
                snapshot: serde_json::json!("CreatedByUs"),
            },
        ],
        config: Some(config_of(instance, port)),
        finished: true,
    }
}

fn write_manifest(path: &Path, state: &InstallState) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, serde_json::to_vec_pretty(state).expect("serialise")).expect("write");
}

/// writes the unnamed instance's manifest under a temp root.
fn write_unnamed(root: &Path, port: u16) -> PathBuf {
    let path = manifest_path_in(root, None);
    write_manifest(&path, &state_of(None, port));
    path
}

/// writes a named instance's manifest under a temp root.
fn write_named(root: &Path, name: &str, port: u16) -> PathBuf {
    let path = manifest_path_in(root, Some(name));
    write_manifest(&path, &state_of(Some(name), port));
    path
}

fn discover(root: &Path) -> invok::manifests::Discovery {
    discover_in(root, &manifest_path_in(root, None), &[])
}

fn ids(found: &[Found]) -> Vec<String> {
    found.iter().map(|f| f.id.to_string()).collect()
}

// --- 1. where each manifest lives -------------------------------------------

#[test]
fn each_instance_has_a_file_of_its_own_and_the_unnamed_one_keeps_its_place() {
    let root = Path::new("/var/lib/invok");
    assert_eq!(
        manifest_path_in(root, None),
        PathBuf::from("/var/lib/invok/state.json"),
        "the unnamed instance's manifest does not move: that path is what every \
         installation in the field already wrote"
    );
    assert_eq!(
        manifest_path_in(root, Some("cliente-x")),
        PathBuf::from("/var/lib/invok/instances/cliente-x.json")
    );
}

// --- 2. discovery -----------------------------------------------------------

#[test]
fn a_machine_with_nothing_installed_yields_nothing_and_no_problem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = discover(dir.path());
    assert!(d.found.is_empty());
    assert!(
        d.problems.is_empty(),
        "an absent instances/ directory is the normal state of a fresh machine, not a problem"
    );
}

#[test]
fn the_unnamed_and_the_named_instances_come_back_in_one_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    write_named(dir.path(), "cliente-x", 8169);
    write_named(dir.path(), "alfa", 8269);

    let d = discover(dir.path());
    assert!(d.problems.is_empty());
    assert_eq!(
        ids(&d.found),
        vec![UNNAMED_ID, "alfa", "cliente-x"],
        "one list, sorted, whatever path each entry came from"
    );
}

/// the case that must never break: an installation made before instances
/// existed, whose manifest sits at one of the historical paths.
///
/// R7 left those paths readable because an instance whose manifest cannot be
/// found is an instance nobody can uninstall. discovery has to keep that
/// promise, not just `rollback`'s old single-path lookup.
#[test]
fn an_installation_from_before_all_this_is_still_found_at_its_historical_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("var-lib-invok");
    let historical = dir.path().join("opt-odoo").join(".installer-state.json");
    write_manifest(&historical, &state_of(None, 8069));

    // the current path does not exist: only the old one does.
    let d = discover_in(
        &root,
        &manifest_path_in(&root, None),
        &[historical.as_path()],
    );

    assert_eq!(ids(&d.found), vec![UNNAMED_ID]);
    assert_eq!(
        d.found[0].path, historical,
        "and it is consumed where it is, not moved: a migration is a mutation of the one \
         file whose loss strands an instance"
    );
}

/// the identity comes from the **manifest**, not from the file name: the undos
/// act through the recorded configuration, so that is what decides.
#[test]
fn a_renamed_file_does_not_rename_the_instance() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = manifest_path_in(dir.path(), Some("whatever-somebody-typed"));
    write_manifest(&path, &state_of(Some("cliente-x"), 8069));

    let d = discover(dir.path());
    assert_eq!(
        ids(&d.found),
        vec!["cliente-x"],
        "the file name is only how it was found; the manifest says what it is"
    );
}

#[test]
fn an_unreadable_manifest_is_a_problem_and_not_an_absence() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_named(dir.path(), "buona", 8069);
    let broken = manifest_path_in(dir.path(), Some("rotta"));
    fs::write(&broken, b"{ this is not json").expect("write");

    let d = discover(dir.path());
    assert_eq!(
        ids(&d.found),
        vec!["buona"],
        "the sound manifests are still found: one corrupt file takes only its own instance down"
    );
    assert_eq!(d.problems.len(), 1);
    assert_eq!(d.problems[0].path, broken);
}

/// without privileges the directory read fails, and the answer must be "I
/// cannot tell", never "there is nothing".
///
/// skipped as root, where the case cannot be reproduced — the same reason
/// `trust_verdict` and `ensure_root_euid` exist as pure functions.
#[test]
fn an_unreadable_directory_is_reported_rather_than_read_as_empty() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: as root the permissions do not bite");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    write_named(dir.path(), "cliente-x", 8069);
    let instances = dir.path().join("instances");
    fs::set_permissions(&instances, fs::Permissions::from_mode(0o000)).expect("chmod");

    let d = discover(dir.path());
    let restored = fs::set_permissions(&instances, fs::Permissions::from_mode(0o755));

    assert!(
        d.found.is_empty() && d.problems.len() == 1,
        "an unreadable instances/ must be reported, not read as an empty machine"
    );
    restored.expect("restore permissions so the tempdir can be cleaned up");
}

// --- 3. picking the instance to undo ----------------------------------------

#[test]
fn with_one_instance_the_command_needs_no_argument() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    let d = discover(dir.path());
    assert_eq!(select(&d.found, None), Selection::One(0));
}

/// bivio 3: with several, it lists them and stops. it does not guess, exactly as
/// it already refuses to guess a configuration it does not have.
#[test]
fn with_several_instances_it_lists_them_and_stops() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    write_named(dir.path(), "cliente-x", 8169);
    let d = discover(dir.path());

    match select(&d.found, None) {
        Selection::Ambiguous(names) => {
            assert_eq!(names, vec![UNNAMED_ID, "cliente-x"]);
        }
        other => panic!("it must stop and list, got {other:?}"),
    }
}

#[test]
fn an_instance_can_be_named_and_the_unnamed_one_is_called_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    write_named(dir.path(), "cliente-x", 8169);
    let d = discover(dir.path());

    let Selection::One(i) = select(&d.found, Some("cliente-x")) else {
        panic!("a named instance must be selectable");
    };
    assert_eq!(d.found[i].id, InstanceId::Named("cliente-x".to_string()));

    let Selection::One(i) = select(&d.found, Some(UNNAMED_ID)) else {
        panic!(
            "the unnamed instance must be selectable too, or it could not be picked \
                on a machine that carries several"
        );
    };
    assert_eq!(d.found[i].id, InstanceId::Unnamed);
}

#[test]
fn asking_for_an_instance_that_is_not_here_says_what_is() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_named(dir.path(), "cliente-x", 8069);
    let d = discover(dir.path());

    match select(&d.found, Some("cliente-y")) {
        Selection::NotFound {
            requested,
            available,
        } => {
            assert_eq!(requested, "cliente-y");
            assert_eq!(available, vec!["cliente-x"]);
        }
        other => panic!("got {other:?}"),
    }
}

/// `default` is reserved, so the selector can never be ambiguous.
#[test]
fn an_instance_may_not_be_called_default() {
    assert!(
        validate_instance(UNNAMED_ID).is_err(),
        "'{UNNAMED_ID}' names the unnamed instance: a real one taking it would make \
         `rollback --instance default` ambiguous"
    );
}

// --- 4. who else is on this machine -----------------------------------------

#[test]
fn the_others_are_counted_and_the_chosen_one_is_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    write_named(dir.path(), "cliente-x", 8169);
    let d = discover(dir.path());

    assert_eq!(d.live_others(&InstanceId::Unnamed), vec!["cliente-x"]);
    assert_eq!(
        d.live_others(&InstanceId::Named("cliente-x".to_string())),
        vec![UNNAMED_ID]
    );
}

/// an empty manifest owns nothing, and must not make the machine look occupied.
///
/// after a complete rollback the file is **removed**, not emptied (R19) — but a
/// file that describes zero artifacts can still be there, and counting it would
/// refuse a rollback to protect an instance that no longer exists.
#[test]
fn an_empty_manifest_does_not_count_as_an_instance_in_use() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    write_manifest(
        &manifest_path_in(dir.path(), Some("svuotata")),
        &InstallState {
            completed: vec![],
            config: Some(config_of(Some("svuotata"), 8169)),
            finished: false,
        },
    );
    let d = discover(dir.path());

    assert_eq!(ids(&d.found).len(), 2, "it is still listed");
    assert!(
        d.live_others(&InstanceId::Unnamed).is_empty(),
        "but it owns nothing, so it does not hold the shared artifacts hostage"
    );
}

// --- 5. the port another instance already claims ----------------------------

/// asked of the manifests, not of the system: a **stopped** instance holds no
/// socket, so its port looks free and would be handed out twice.
#[test]
fn a_port_recorded_by_another_instance_is_a_conflict_even_with_nothing_listening() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    let d = discover(dir.path());

    let conflict = port_conflict(&d.found, &InstanceId::Named("cliente-x".to_string()), 8069);
    assert_eq!(conflict, Some((UNNAMED_ID.to_string(), 8069)));

    assert_eq!(
        port_conflict(&d.found, &InstanceId::Named("cliente-x".to_string()), 8169),
        None,
        "a different port is not a conflict"
    );
}

#[test]
fn an_instance_does_not_conflict_with_itself() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_unnamed(dir.path(), 8069);
    let d = discover(dir.path());
    assert_eq!(
        port_conflict(&d.found, &InstanceId::Unnamed, 8069),
        None,
        "resuming or re-running an instance must not trip over its own recorded port"
    );
}

// --- 6. the command line ----------------------------------------------------

#[test]
fn the_rollback_takes_an_instance_and_list_exists() {
    use clap::Parser;
    use invok::cli::{Cli, Command};

    let parsed = Cli::try_parse_from(["invok", "rollback", "--instance", "cliente-x"])
        .expect("rollback --instance must be accepted");
    match parsed.command {
        Some(Command::Rollback(args)) => assert_eq!(args.instance.as_deref(), Some("cliente-x")),
        other => panic!("got {other:?}"),
    }

    // the historical invocation is untouched: no subcommand still installs, and
    // `rollback` with no argument still works on a machine with one instance.
    let bare = Cli::try_parse_from(["invok", "rollback"]).expect("still valid");
    match bare.command {
        Some(Command::Rollback(args)) => assert_eq!(args.instance, None),
        other => panic!("got {other:?}"),
    }
    assert!(Cli::try_parse_from(["invok"])
        .expect("no subcommand still installs")
        .command
        .is_none());

    assert!(matches!(
        Cli::try_parse_from(["invok", "list"])
            .expect("list must exist")
            .command,
        Some(Command::List)
    ));
    assert!(matches!(
        Cli::try_parse_from(["invok", "uninstall"])
            .expect("the alias survives")
            .command,
        Some(Command::Rollback(_))
    ));
}

/// `--state` and `--instance` answer the same question in two ways, and a
/// command that took both would have to decide which one loses — on a
/// destructive operation. it refuses instead.
///
/// this test earned its place immediately: declared on `--state` as
/// `conflicts_with = "instance"`, the exclusion was **silently dropped** by
/// clap, which ignores a reference to an argument it has not built yet. the
/// flag pair parsed happily and the guard never existed. asserting the
/// behaviour, not the attribute, is what caught it.
#[test]
fn state_and_instance_cannot_be_given_together() {
    use clap::Parser;
    use invok::cli::Cli;

    assert!(
        Cli::try_parse_from([
            "invok",
            "rollback",
            "--instance",
            "cliente-x",
            "--state",
            "/tmp/altro.json"
        ])
        .is_err(),
        "two ways of naming the manifest, given at once, must be refused rather than ranked"
    );
}
