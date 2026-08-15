//! phase I0: `--instance` and every name derived from it.
//!
//! two things are being defended here, and they pull in opposite directions.
//!
//! 1. **the unnamed instance must not change by one byte.** whoever installed
//!    with 3.0.0 and re-runs the next version must find the same unit, the same
//!    config file, the same user, the same database, the same helper — because
//!    the manifest's identity check compares exactly those names, and a rename
//!    would make an installed instance unrecognisable, therefore not resumable
//!    and not uninstallable.
//! 2. **two named instances must share nothing.** every artifact carries the
//!    instance's name, so that two installations of the *same Odoo version* —
//!    the case the version-based naming forbade forever — can coexist.
//!
//! the tests are written as those two sentences, not as "does the format string
//! work".

use std::path::PathBuf;

use invok::config::{ConfigError, RawConfig, ResolvedConfig};
use invok::context::Context;
use invok::instance::{
    artifact_base, qualified_name, validate_instance, INSTANCE_PREFIX, MAX_INSTANCE_LEN,
};
use invok::state::InstallConfig;

/// a baseline CLI config with a non-default password, so non-interactive
/// resolution does not trip the master-password hard stop.
fn cli_base() -> RawConfig {
    RawConfig {
        admin_passwd: Some("s3cret".to_string()),
        ..Default::default()
    }
}

fn resolve(cli: &RawConfig) -> ResolvedConfig {
    ResolvedConfig::resolve(
        cli,
        &RawConfig::default(),
        &RawConfig::default(),
        /* interactive */ false,
    )
    .expect("resolution")
}

fn ctx_of(config: ResolvedConfig) -> Context {
    Context::from_resolved(config, false, PathBuf::from("/var/lib/invok/state.json"))
}

// --- 1. the unnamed instance is untouched -----------------------------------

/// the whole I0 contract in one assertion set: with no `--instance`, every
/// derived name is what it was before instances existed.
#[test]
fn without_an_instance_every_name_is_the_historical_one() {
    let ctx = ctx_of(resolve(&cli_base()));

    assert_eq!(
        ctx.instance, None,
        "no --instance means the unnamed instance"
    );
    assert_eq!(ctx.artifact_base(), "odoo18");
    assert_eq!(ctx.qualified_name(), "odoo");
    assert_eq!(ctx.odoo_user, "odoo");
    assert_eq!(ctx.db_user, "odoo");
    assert_eq!(ctx.db_name, "odoo");
    assert_eq!(ctx.install_dir, PathBuf::from("/opt/odoo/odoo18"));
    assert_eq!(
        ctx.user_home(),
        PathBuf::from("/opt/odoo"),
        "the unnamed instance's home IS the shared root, as it always was"
    );
    assert_eq!(
        invok::steps::generate_config::data_dir(&ctx),
        PathBuf::from("/opt/odoo/.local/share/Odoo"),
        "the filestore does not move for an installation that never asked for an instance"
    );
}

/// the version still drives the unnamed instance's names — it is the only thing
/// that can, since there is no instance name to use.
#[test]
fn without_an_instance_the_version_still_names_things() {
    let cli = RawConfig {
        version: Some("17".to_string()),
        ..cli_base()
    };
    let ctx = ctx_of(resolve(&cli));
    assert_eq!(ctx.artifact_base(), "odoo17");
    assert_eq!(ctx.install_dir, PathBuf::from("/opt/odoo/odoo17"));
    assert_eq!(
        ctx.qualified_name(),
        "odoo",
        "the user and the database were never versioned, and still are not"
    );
}

// --- 2. a named instance qualifies everything -------------------------------

#[test]
fn a_named_instance_carries_its_name_into_every_artifact() {
    let cli = RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    };
    let ctx = ctx_of(resolve(&cli));

    assert_eq!(ctx.artifact_base(), "odoo-cliente-x");
    assert_eq!(ctx.qualified_name(), "odoo-cliente-x");
    assert_eq!(ctx.odoo_user, "odoo-cliente-x");
    assert_eq!(
        ctx.db_user, ctx.odoo_user,
        "the role must be named exactly like the system user, or `peer` refuses the login"
    );
    assert_eq!(ctx.db_name, "odoo-cliente-x");
    assert_eq!(ctx.install_dir, PathBuf::from("/opt/odoo/odoo-cliente-x"));
    assert_eq!(
        ctx.user_home(),
        ctx.install_dir,
        "a named instance's home is its own directory, not the shared root"
    );
}

/// the isolation claim, at the level a mock can check it: the filestore and the
/// cache of a named instance live **inside** that instance's perimeter.
///
/// this is what makes per-instance PostgreSQL roles mean something. with a
/// single shared home the attachments of two customers would sit side by side
/// under one user, and the separation would be nominal.
#[test]
fn a_named_instance_keeps_its_data_inside_its_own_perimeter() {
    let cli = RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    };
    let ctx = ctx_of(resolve(&cli));

    let filestore = invok::steps::generate_config::data_dir(&ctx);
    assert!(
        filestore.starts_with(&ctx.install_dir),
        "the filestore must be inside the instance, was {}",
        filestore.display()
    );
    assert!(
        invok::steps::setup_cache_dir::SetupCacheDir::cache_dir(&ctx).starts_with(&ctx.install_dir),
        "the cache must be inside the instance too"
    );
}

/// the case version-based naming made impossible: two instances of the **same**
/// Odoo version. nothing they own may collide.
#[test]
fn two_instances_of_the_same_version_share_no_artifact() {
    let a = ctx_of(resolve(&RawConfig {
        instance: Some("alfa".to_string()),
        version: Some("18".to_string()),
        ..cli_base()
    }));
    let b = ctx_of(resolve(&RawConfig {
        instance: Some("beta".to_string()),
        version: Some("18".to_string()),
        ..cli_base()
    }));

    assert_ne!(a.artifact_base(), b.artifact_base(), "unit and config file");
    assert_ne!(a.install_dir, b.install_dir, "sources and virtualenv");
    assert_ne!(a.user_home(), b.user_home(), "home, filestore and cache");
    assert_ne!(a.odoo_user, b.odoo_user, "system user");
    assert_ne!(a.db_user, b.db_user, "PostgreSQL role");
    assert_ne!(a.db_name, b.db_name, "database");
    assert_ne!(
        invok::steps::generate_config::data_dir(&a),
        invok::steps::generate_config::data_dir(&b),
        "the filestore: this is the one where a collision destroys data"
    );
}

// --- 3. the name is validated before anything is mutated --------------------

#[test]
fn a_well_formed_name_is_accepted() {
    for name in ["a", "cliente-x", "c1", "acme_srl", "x".repeat(26).as_str()] {
        assert!(
            validate_instance(name).is_ok(),
            "'{name}' should be a valid instance name"
        );
    }
}

#[test]
fn a_malformed_name_is_refused_before_anything_is_touched() {
    // each of these breaks a *different* one of the five grammars the name ends
    // up in, which is the reason the check exists at all (A-V6-1).
    for (name, why) in [
        ("", "empty"),
        ("Cliente", "uppercase: PostgreSQL folds it, a path does not"),
        ("1cliente", "leading digit: not a valid systemd unit name"),
        ("-cliente", "leading dash: read as an option by useradd"),
        ("cliente x", "a space, in a unit name and a path"),
        ("cliente.x", "a dot, which systemd gives its own meaning"),
        (
            "cliente/x",
            "a separator, in something used as a path component",
        ),
        ("cliente$x", "shell metacharacter"),
    ] {
        assert!(
            matches!(
                validate_instance(name),
                Err(ConfigError::InvalidInstance { .. })
            ),
            "'{name}' must be refused ({why})"
        );
    }
}

/// the length limit is not a style rule: it is `UT_NAMESIZE` minus the prefix.
///
/// tying the constants together here means that raising `MAX_INSTANCE_LEN`
/// without thinking about the user name fails a test instead of producing, in
/// the field, a truncated user with an untruncated role — which breaks `peer`
/// silently.
#[test]
fn the_length_limit_is_the_one_a_unix_user_name_imposes() {
    /// what a Unix user name may not exceed.
    const UT_NAMESIZE: usize = 32;

    assert!(
        INSTANCE_PREFIX.len() + MAX_INSTANCE_LEN < UT_NAMESIZE,
        "'{INSTANCE_PREFIX}' + {MAX_INSTANCE_LEN} characters must stay under {UT_NAMESIZE}"
    );

    let longest = "a".repeat(MAX_INSTANCE_LEN);
    assert!(validate_instance(&longest).is_ok());
    assert!(qualified_name(Some(&longest)).len() < UT_NAMESIZE);

    let too_long = "a".repeat(MAX_INSTANCE_LEN + 1);
    assert!(
        matches!(
            validate_instance(&too_long),
            Err(ConfigError::InvalidInstance { .. })
        ),
        "one character over the limit must be refused, not truncated"
    );
}

/// the name is validated during config resolution, which is *before* the
/// preflight and long before any mutation. a bad name must never reach a step.
#[test]
fn resolution_refuses_a_bad_name_rather_than_carrying_it_into_the_steps() {
    let cli = RawConfig {
        instance: Some("Cliente X".to_string()),
        ..cli_base()
    };
    let err = ResolvedConfig::resolve(
        &cli,
        &RawConfig::default(),
        &RawConfig::default(),
        /* interactive */ false,
    )
    .expect_err("a malformed instance name must stop the resolution");
    assert!(
        matches!(err, ConfigError::InvalidInstance { .. }),
        "got {err:?}"
    );
}

/// an empty value means "unnamed", not "an instance whose name is nothing".
///
/// `ODOO_INSTANCE=` in an `.env` is how a customer disables a line without
/// deleting it, and it must read as the historical behaviour rather than as an
/// error or, worse, as a nameless instance.
#[test]
fn an_empty_name_reads_as_the_unnamed_instance() {
    let ctx = ctx_of(resolve(&RawConfig {
        instance: Some(String::new()),
        ..cli_base()
    }));
    assert_eq!(ctx.instance, None);
    assert_eq!(ctx.artifact_base(), "odoo18");
}

// --- 4. explicit overrides still win ----------------------------------------

/// the instance only supplies **defaults**. somebody who names the user, the
/// database or the install dir explicitly still gets what they asked for — the
/// cascade is unchanged.
#[test]
fn explicit_values_still_beat_the_instance_defaults() {
    let cli = RawConfig {
        instance: Some("cliente-x".to_string()),
        odoo_user: Some("custom".to_string()),
        db_name: Some("customdb".to_string()),
        install_dir: Some("/opt/odoo/altrove".to_string()),
        ..cli_base()
    };
    let ctx = ctx_of(resolve(&cli));
    assert_eq!(ctx.odoo_user, "custom");
    assert_eq!(ctx.db_name, "customdb");
    assert_eq!(ctx.install_dir, PathBuf::from("/opt/odoo/altrove"));
    assert_eq!(
        ctx.db_user, "custom",
        "the role follows the system user, whatever named it"
    );
}

// --- 5. the manifest records the instance -----------------------------------

/// the name must be **re-read** from the manifest, not re-derived: every undo
/// names its artifact through it.
#[test]
fn the_manifest_carries_the_instance_and_gives_it_back() {
    let ctx = ctx_of(resolve(&RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    }));
    let config = InstallConfig::from_context(&ctx);
    assert_eq!(config.instance.as_deref(), Some("cliente-x"));

    let rebuilt = config.to_context(false, false, PathBuf::from("/tmp/state.json"));
    assert_eq!(rebuilt.instance.as_deref(), Some("cliente-x"));
    assert_eq!(
        rebuilt.artifact_base(),
        "odoo-cliente-x",
        "a rollback from disk must reach the same unit the installation created"
    );
    assert_eq!(rebuilt.user_home(), rebuilt.install_dir);
}

/// a manifest written before I0 has no instance field, and must read as the
/// unnamed instance — which is the truth about every installation that exists
/// today. anything else would make those instances un-uninstallable.
#[test]
fn a_manifest_written_before_instances_reads_as_the_unnamed_one() {
    let json = serde_json::json!({
        "odoo_version": "18.0",
        "odoo_version_short": "18",
        "odoo_user": "odoo",
        "db_user": "odoo",
        "db_name": "odoo",
        "odoo_home": "/opt/odoo",
        "install_dir": "/opt/odoo/odoo18",
        "port": 8069,
        "odoo_logfile": null,
        "with_nginx": false,
        "sudo_user": "admin",
    });
    let config: InstallConfig = serde_json::from_value(json).expect("a pre-I0 manifest must load");
    assert_eq!(config.instance, None);

    let ctx = config.to_context(false, false, PathBuf::from("/tmp/state.json"));
    assert_eq!(ctx.artifact_base(), "odoo18");
    assert_eq!(ctx.user_home(), PathBuf::from("/opt/odoo"));
}

/// the instance names artifacts, so it belongs in the identity the resume
/// compares (A-V3-1's rule applied to I0): resuming a manifest under a different
/// instance would undo somebody else's unit.
#[test]
fn the_instance_is_part_of_the_identity_a_resume_compares() {
    let unnamed = InstallConfig::from_context(&ctx_of(resolve(&cli_base())));
    let named = InstallConfig::from_context(&ctx_of(resolve(&RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    })));

    assert!(!unnamed.same_identity(&named));
    assert!(
        unnamed
            .identity()
            .iter()
            .any(|(field, _)| *field == "instance"),
        "the identity must name the instance, so the refusal can say which field differs"
    );
}

// --- 6. the rendered artifacts ----------------------------------------------

/// the unit is where two instances of one version used to collide in three
/// places at once: its file name, its syslog identity and the config it loads.
#[test]
fn the_rendered_unit_is_named_after_the_instance() {
    let ctx = ctx_of(resolve(&RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    }));
    let unit = invok::steps::setup_systemd::render_unit(&ctx);

    assert!(unit.contains("SyslogIdentifier=odoo-cliente-x"));
    assert!(unit.contains("/opt/odoo/odoo-cliente-x/odoo-cliente-x.conf"));
    assert!(unit.contains("RuntimeDirectory=odoo-cliente-x"));
    assert!(unit.contains("User=odoo-cliente-x"));
    assert!(
        !unit.contains("{{"),
        "no placeholder may survive: an unsubstituted unit fails to start"
    );
}

/// `/run/odoo` belongs to the other family: the unnamed instance keeps it.
///
/// it is not cosmetic. systemd **removes** a `RuntimeDirectory` when the
/// service stops, so two units declaring the same one would have each stop pull
/// the ground from under the other.
#[test]
fn the_runtime_directory_is_unchanged_for_the_unnamed_instance() {
    let unit = invok::steps::setup_systemd::render_unit(&ctx_of(resolve(&cli_base())));
    assert!(unit.contains("RuntimeDirectory=odoo\n"));
    assert!(unit.contains("SyslogIdentifier=odoo18"));
    assert!(unit.contains("/opt/odoo/odoo18/odoo18.conf"));
}

/// nginx writes one log pair per instance. before I0 it was one per *version*,
/// so two instances of the same version wrote into the same files (the
/// multi-instance shape of A-V3-12).
#[test]
fn the_vhost_logs_are_named_after_the_instance() {
    let ctx = ctx_of(resolve(&RawConfig {
        instance: Some("cliente-x".to_string()),
        with_nginx: Some(true),
        ..cli_base()
    }));
    let vhost = invok::steps::nginx_write_config::render_vhost(&ctx);
    assert!(vhost.contains("/var/log/nginx/odoo-cliente-x.access.log"));
    assert!(vhost.contains("/var/log/nginx/odoo-cliente-x.error.log"));
    assert!(!vhost.contains("{{"));
}

#[test]
fn the_vhost_logs_are_unchanged_for_the_unnamed_instance() {
    let ctx = ctx_of(resolve(&RawConfig {
        with_nginx: Some(true),
        ..cli_base()
    }));
    let vhost = invok::steps::nginx_write_config::render_vhost(&ctx);
    assert!(vhost.contains("/var/log/nginx/odoo18.access.log"));
    assert!(vhost.contains("/var/log/nginx/odoo18.error.log"));
}

// --- 7. the pure naming rules, directly -------------------------------------

/// the two families exist because they disagree on exactly one case: the
/// unnamed instance. that disagreement is the whole reason there are two
/// functions and not one.
#[test]
fn the_two_name_families_differ_only_for_the_unnamed_instance() {
    assert_eq!(artifact_base(None, "18"), "odoo18");
    assert_eq!(qualified_name(None), "odoo");
    assert_ne!(artifact_base(None, "18"), qualified_name(None));

    assert_eq!(artifact_base(Some("x"), "18"), "odoo-x");
    assert_eq!(qualified_name(Some("x")), "odoo-x");
    assert_eq!(
        artifact_base(Some("x"), "18"),
        qualified_name(Some("x")),
        "for a named instance the two collapse: that is what qualifies every artifact"
    );
}

/// a named instance's names must not depend on the Odoo version — otherwise
/// upgrading an instance in place would rename its unit, its database and its
/// user, and the manifest would no longer recognise it.
#[test]
fn a_named_instance_does_not_change_name_with_the_version() {
    assert_eq!(
        artifact_base(Some("cliente-x"), "17"),
        artifact_base(Some("cliente-x"), "18")
    );
}

// --- 8. the two ways of asking for an instance ------------------------------

/// `--instance` on the command line and `ODOO_INSTANCE` in an `.env` must reach
/// the same place: the `.env` is how a customer's configuration is kept in a
/// file, and a flag with no file equivalent would force them to script it.
#[test]
fn the_instance_arrives_from_the_cli_and_from_the_env_file() {
    use clap::Parser;
    use std::io::Write;

    let parsed = invok::cli::Cli::try_parse_from(["invok", "--instance", "cliente-x"])
        .expect("--instance must be accepted");
    assert_eq!(parsed.instance.as_deref(), Some("cliente-x"));

    let without = invok::cli::Cli::try_parse_from(["invok"]).expect("no flag is still valid");
    assert_eq!(
        without.instance, None,
        "the flag stays optional: that is what keeps every existing invocation working"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cliente.env");
    let mut f = std::fs::File::create(&path).expect("create");
    writeln!(f, "ODOO_INSTANCE=\"cliente-x\"").expect("write");
    let from_file = invok::config::parse_env_file(&path).expect("parse");
    assert_eq!(from_file.instance.as_deref(), Some("cliente-x"));
}

// --- 9. the config path has one author --------------------------------------

/// the unit must load **exactly** the file the config step writes.
///
/// they used to be two independent `format!`s (three, with the database init),
/// and the failure mode is quiet: the service starts against a file that is not
/// there, or `odoo-bin -i base` initialises with settings the service will not
/// use. I0 gave the divergence a way to happen, so the path now has one author
/// and this test is what says so.
#[test]
fn the_unit_loads_exactly_the_config_the_installer_writes() {
    for instance in [None, Some("cliente-x")] {
        let ctx = ctx_of(resolve(&RawConfig {
            instance: instance.map(str::to_string),
            ..cli_base()
        }));
        let written = invok::steps::generate_config::config_path(&ctx);
        let unit = invok::steps::setup_systemd::render_unit(&ctx);
        assert!(
            unit.contains(&format!("-c {}", written.display())),
            "the unit must load {}, and it says:\n{unit}",
            written.display()
        );
    }
}

/// and the config file is named after the instance, not the version — which is
/// what lets two instances of one version each have their own.
#[test]
fn the_config_file_is_named_after_the_instance() {
    let named = ctx_of(resolve(&RawConfig {
        instance: Some("cliente-x".to_string()),
        ..cli_base()
    }));
    assert_eq!(
        invok::steps::generate_config::config_path(&named),
        PathBuf::from("/opt/odoo/odoo-cliente-x/odoo-cliente-x.conf")
    );

    let unnamed = ctx_of(resolve(&cli_base()));
    assert_eq!(
        invok::steps::generate_config::config_path(&unnamed),
        PathBuf::from("/opt/odoo/odoo18/odoo18.conf"),
        "and the unnamed instance's config does not move"
    );
}

// --- 10. the structural guard -----------------------------------------------

/// nobody may re-derive an artifact name from the version by hand.
///
/// the guard is structural because the behavioural tests above can only cover
/// the call sites that exist **today**: a new step formatting `odoo{version}`
/// would name an artifact the instance does not own, and — since the manifest
/// records what the step reports — nothing at rollback time would notice. the
/// rule is the one already written for the filestore: two identical `format!`s
/// in two files are the premise of a rollback cleaning the wrong directory.
///
/// paired with the behavioural tests deliberately (the R9 lesson): the grep
/// sees the *shape* of the code, the tests see the *name* that comes out of it.
#[test]
fn no_artifact_name_is_derived_from_the_version_by_hand() {
    use std::fs;
    use std::path::Path;

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src];
    let mut checked = 0usize;
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().map(|e| e != "rs").unwrap_or(true) {
                continue;
            }
            // the one module allowed to know how the names are spelled.
            if path.ends_with("instance.rs") {
                continue;
            }
            let content = fs::read_to_string(&path).expect("read");
            checked += 1;
            for (n, line) in content.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                assert!(
                    !code.contains("format!(\"odoo{"),
                    "{}:{} builds an artifact name out of the version.\n  {}\n\
                     use Context::artifact_base (unit, config, vhost, install dir) or \
                     Context::qualified_name (user, role, database, helper): with instances \
                     the version no longer decides those names",
                    path.display(),
                    n + 1,
                    code.trim()
                );
            }
        }
    }
    assert!(
        checked > 20,
        "the guard must have actually read the sources"
    );
}
