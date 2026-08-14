//! M2: the guarantee that the critical protections **do not depend on the
//! family**.
//!
//! the multi-distro design promises the database anti-drop, the init hard stop,
//! the `.bashrc` care and the filestore rule stay intact under both backends.
//! that promise used to be an analysis — those steps do not call the package
//! manager, so they cannot depend on it. with two real families it becomes
//! **checkable**.
//!
//! analysis is not enough because nothing stops someone reading the family
//! inside a protected step "just for a log" and then branching on it. that
//! defect would be invisible in every step test, which runs on one family, and
//! would surface in the field as "the rollback did not drop the database".
//!
//! what is compared: the **sequence of operations** reaching the system, and
//! the **persisted snapshot** — respectively what is done and what the undo
//! will be able to undo.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::OsFamily;
use invok::step::Step;

fn ctx(family: OsFamily) -> Context {
    Context {
        dry_run: false,
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        odoo_home: "/opt/odoo".into(),
        install_dir: "/opt/odoo/odoo18".into(),
        odoo_version_short: "18".to_string(),
        os_family: family,
        ..Default::default()
    }
}

fn cfg(family: OsFamily) -> MockConfig {
    MockConfig {
        family,
        ..MockConfig::default()
    }
}

/// replaces the **random** suffixes of temporary files with a placeholder.
///
/// temporaries are unpredictable by construction (A-V3-3), so two runs of the
/// same step always differ there. without this the cross-family comparison
/// would fail for an unrelated reason — as it did on this test's first run,
/// which is also proof that the comparison looks at values and not just shape.
fn senza_suffissi_casuali(op: &Op) -> String {
    let reso = format!("{op:?}");
    let mut out = String::with_capacity(reso.len());
    let mut hex = String::new();

    let scarica = |out: &mut String, hex: &mut String| {
        // the suffix is a long unbroken run of hex digits; a normal word is
        // not.
        if hex.len() >= 12 {
            out.push_str("<rnd>");
        } else {
            out.push_str(hex);
        }
        hex.clear();
    };

    for c in reso.chars() {
        if c.is_ascii_hexdigit() {
            hex.push(c);
        } else {
            scarica(&mut out, &mut hex);
            out.push(c);
        }
    }
    scarica(&mut out, &mut hex);
    out
}

/// runs one step's full cycle for one family, returning what the system saw and
/// what was persisted.
fn esegui<S: Step>(
    costruisci: impl Fn(Box<dyn invok::system_ops::SystemOps>) -> S,
    family: OsFamily,
) -> (Vec<String>, serde_json::Value) {
    let (ops, log) = MockSystemOps::new(cfg(family));
    let c = ctx(family);
    let mut step = costruisci(Box::new(ops));

    // errors are not propagated: some steps fail by mock configuration, and
    // what matters is that they fail **the same way** on both.
    let _ = step.snapshot(&c);
    let _ = step.run(&c);
    let _ = step.undo(&c);

    let ops: Vec<String> = ops_of(&log).iter().map(senza_suffissi_casuali).collect();
    (ops, step.snapshot_value())
}

/// the heart: a step must do **the same things** on both families.
fn indifferente_alla_famiglia<S: Step>(
    nome: &str,
    costruisci: impl Fn(Box<dyn invok::system_ops::SystemOps>) -> S,
) {
    let (ops_debian, snap_debian) = esegui(&costruisci, OsFamily::Debian);
    let (ops_fedora, snap_fedora) = esegui(&costruisci, OsFamily::Fedora);

    assert_eq!(
        ops_debian, ops_fedora,
        "'{nome}' si comporta diversamente a seconda della famiglia: non deve. \
         Se questo step ha iniziato a leggere `ctx.os_family`, la protezione che \
         presidia ha smesso di essere distro-indipendente"
    );
    assert_eq!(
        snap_debian, snap_fedora,
        "'{nome}' persiste uno snapshot diverso a seconda della famiglia: l'undo \
         di domani vedrebbe due verità diverse per lo stesso artefatto"
    );
}

/// **the four critical protections**, plus the steps around them.
///
/// the list is the part that matters: each entry is a step the design declares
/// distro-independent. adding one here is how you declare it so — and find out
/// at once if it is not.
#[test]
fn the_critical_protections_do_not_depend_on_the_family() {
    use invok::steps::*;

    // the database anti-drop: the verdict is the `PreState`.
    indifferente_alla_famiglia("create-database", |o| {
        create_database::CreateDatabase::with_ops(o)
    });
    // the init hard stop: a precondition on the shared verdict.
    indifferente_alla_famiglia("initialize-odoo-database", |o| {
        initialize_odoo_database::InitializeOdooDatabase::with_ops(o)
    });
    // the `.bashrc` care: pure filesystem, a backup and a single line.
    indifferente_alla_famiglia("patch-bashrc", patch_bashrc::PatchBashrc::with_ops);
    // the filestore: a double condition, ours **and** our database.
    indifferente_alla_famiglia("setup-data-dir", |o| {
        setup_data_dir::SetupDataDir::with_ops(o)
    });

    // the PostgreSQL role and the rest of the reversible perimeter.
    indifferente_alla_famiglia("create-db-role", |o| {
        create_db_role::CreateDbRole::with_ops(o)
    });
    indifferente_alla_famiglia("prepare-opt-root", |o| {
        prepare_opt_root::PrepareOptRoot::with_ops(o)
    });
    indifferente_alla_famiglia("create-odoo-user", |o| {
        create_odoo_user::CreateOdooUser::with_ops(o)
    });
    indifferente_alla_famiglia("setup-cache-dir", |o| {
        setup_cache_dir::SetupCacheDir::with_ops(o)
    });
    indifferente_alla_famiglia("setup-log-dir", setup_log_dir::SetupLogDir::with_ops);
    indifferente_alla_famiglia("clone-odoo-repo", |o| {
        clone_odoo_repo::CloneOdooRepo::with_ops(o)
    });
    indifferente_alla_famiglia("create-virtualenv", |o| {
        create_virtualenv::CreateVirtualenv::with_ops(o)
    });
    indifferente_alla_famiglia("install-python-requirements", |o| {
        install_python_requirements::InstallPythonRequirements::with_ops(o)
    });
    indifferente_alla_famiglia("generate-config", |o| {
        generate_config::GenerateConfig::with_ops(o)
    });
    indifferente_alla_famiglia("setup-systemd", |o| {
        setup_systemd::SetupSystemd::with_ops(o)
    });
    indifferente_alla_famiglia("write-control-script", |o| {
        write_control_script::WriteControlScript::with_ops(o)
    });
}

/// the counterweight: the steps that **must** depend on the family really do.
///
/// without this, the test above would pass even if multi-distro support did not
/// work at all — "every step behaves the same" is exactly what you get when the
/// family never reaches its destination.
#[test]
fn the_packaging_steps_do_depend_on_the_family() {
    use invok::steps::apt_packages::AptPackagesStep;

    let (ops_debian, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Debian,
    );
    let (ops_fedora, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Fedora,
    );

    assert_ne!(
        ops_debian, ops_fedora,
        "install-system-dependencies deve installare pacchetti DIVERSI sulle due \
         famiglie: se sono uguali, la famiglia non è arrivata fino al catalogo"
    );

    let reso = |ops: &[String]| ops.join(" ");

    assert!(
        reso(&ops_debian).contains("build-essential"),
        "su Debian il compilatore arriva col metapacchetto"
    );
    assert!(
        reso(&ops_fedora).contains("gcc-c++"),
        "su Fedora servono i nomi espliciti: build-essential non esiste"
    );
    assert!(
        !reso(&ops_fedora).contains("build-essential"),
        "un nome Debian nella lista Fedora bloccherebbe l'installazione nello \
         snapshot — che è il comportamento giusto, ma qui vuol dire lista non tradotta"
    );
}

/// the delta contains **no** duplicates even where two needs fall on the same
/// package (A-MD-1 in structural form).
#[test]
fn the_fedora_delta_has_no_duplicates() {
    use invok::steps::apt_packages::AptPackagesStep;

    let (ops, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Fedora,
    );

    // the packages handed to the install, taken from the rendered operation.
    let riga = ops
        .iter()
        .find(|o| o.starts_with("PkgInstall"))
        .cloned()
        .unwrap_or_default();
    let installati: Vec<String> = riga
        .trim_start_matches("PkgInstall([")
        .trim_end_matches("])")
        .split(", ")
        .map(|n| n.trim_matches('"').to_string())
        .filter(|n| !n.is_empty())
        .collect();

    let mut visti = std::collections::HashSet::new();
    let doppi: Vec<&String> = installati.iter().filter(|n| !visti.insert(*n)).collect();

    assert!(
        doppi.is_empty(),
        "sulla famiglia Fedora più bisogni cadono sullo stesso pacchetto \
         (i due jpeg): il delta deve elencarlo una volta sola, trovati {doppi:?}"
    );
}
