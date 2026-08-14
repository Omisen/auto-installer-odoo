//! M1: extracting the two boundaries, and the parts the extraction really
//! changed.
//!
//! most of M1 is a refactor, and the guarantee that it was neutral is the
//! pre-existing suite passing unchanged. what lives here is what is **not**
//! unchanged:
//!
//! - the backend choice, now a decision taken in one place that can say no;
//! - deduplication of resolved names (A-MD-1), a defect found while writing the
//!   design;
//! - the availability decision, extracted as a pure function because mutation
//!   testing found it uncovered;
//! - the distinction between production and test constructors, which removing
//!   the old `new()` was about to lose.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::OsFamily;
use invok::step::Step;
use invok::steps::apt_packages::{dedup_keeping_order, AptPackagesStep, UndoPolicy};
use invok::system_ops::backend_factory;

fn ctx() -> Context {
    Context {
        dry_run: false,
        ..Default::default()
    }
}

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// --- the backend choice: one decision, in one place -------------------------

/// **both** families have a backend, and neither gets the other's.
///
/// the check is not "a factory exists" — true even if both returned the same
/// one — but that the commands produced are the right ones. that error would be
/// silent, and in a rollback it would mean leaving everything installed.
#[test]
fn each_family_gets_its_own_backend() {
    let debian = backend_factory(OsFamily::Debian).expect("la famiglia Debian ha apt")();
    let fedora = backend_factory(OsFamily::Fedora).expect("la famiglia Fedora ha dnf")();

    assert_eq!(
        debian.packages().catalog().postgres_marker,
        "postgresql",
        "su Debian il marker del server è il pacchetto `postgresql`"
    );
    assert_eq!(
        fedora.packages().catalog().postgres_marker,
        "postgresql-server",
        "su Fedora `postgresql` è il solo CLIENT: prenderlo come marker farebbe \
         credere che il server ci sia già, quindi Preexisting, quindi nessun undo"
    );
}

/// the factory's default is the family of **every existing installation**.
#[test]
fn the_default_family_is_the_one_every_existing_manifest_describes() {
    assert!(backend_factory(OsFamily::default()).is_some());
}

/// the catalogue is what the backend answers: the list is no longer a constant
/// a step reads on its own.
#[test]
fn the_package_lists_come_from_the_backend_catalog() {
    let ops = MockSystemOps::new(MockConfig::default()).0;
    let catalog = invok::system_ops::SystemOps::packages(&ops).catalog();

    assert!(
        catalog
            .bootstrap_specs()
            .iter()
            .any(|s| s.preferred() == "git"),
        "il bootstrap della famiglia Debian contiene git"
    );
    assert!(
        catalog
            .odoo_specs()
            .iter()
            .any(|s| s.preferred() == "build-essential"),
        "le dipendenze Odoo della famiglia Debian contengono build-essential"
    );
    assert_eq!(catalog.postgres_marker, "postgresql");
    assert_eq!(catalog.nginx, "nginx");
    assert!(
        catalog.postgres.contains(&"postgresql-contrib".to_string()),
        "il server PostgreSQL su Debian sono due pacchetti, non uno"
    );
}

// --- A-MD-1: the persisted delta holds no duplicates ------------------------

/// the pure function, on the cases that matter.
#[test]
fn dedup_keeps_the_first_occurrence_and_the_order() {
    let mut v = names(&["git", "libjpeg-dev", "curl", "libjpeg-dev", "wget"]);
    dedup_keeping_order(&mut v);
    assert_eq!(v, names(&["git", "libjpeg-dev", "curl", "wget"]));

    // the plain `dedup` would not do: the duplicates are not adjacent. this is
    // the real case — in one catalogue the two are six positions apart.
    let mut consecutivi = names(&["a", "a", "b"]);
    dedup_keeping_order(&mut consecutivi);
    assert_eq!(consecutivi, names(&["a", "b"]));

    let mut vuoto: Vec<String> = Vec::new();
    dedup_keeping_order(&mut vuoto);
    assert!(vuoto.is_empty());
}

/// **the defect, as it shows up.** two groups resolving to the same name put
/// that name in the persisted delta **twice**.
///
/// harmless to the commands, which are idempotent — but the delta is the
/// accounting of what we added and the only thing the undo may act on. a
/// double-counted ledger is a wrong ledger, and on a family where several
/// groups collapse onto one name it stops being an edge case.
#[test]
fn two_groups_resolving_to_the_same_name_appear_once_in_the_delta() {
    let (ops, _log) = MockSystemOps::new(MockConfig {
        // one name is absent on this "release": the second group falls back to
        // the third alternative, which is the first group's too.
        packages_without_candidate: ["libjpeg8-dev", "libjpeg-turbo8-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            invok::packaging::PackageSpec::one("libjpeg-dev"),
            invok::packaging::PackageSpec::any(&[
                "libjpeg8-dev",
                "libjpeg-turbo8-dev",
                "libjpeg-dev",
            ]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");

    let snap: invok::steps::apt_packages::AptDeltaSnapshot =
        serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile");

    assert_eq!(
        snap.delta,
        names(&["libjpeg-dev"]),
        "il delta persistito deve nominare ogni pacchetto UNA volta: è la \
         contabilità su cui l'undo agisce"
    );
}

/// the same for the pre-existing ones: both groups recognise a package that was
/// already there, and it must be listed once.
#[test]
fn a_preexisting_package_shared_by_two_groups_is_listed_once() {
    let (ops, _log) = MockSystemOps::new(MockConfig {
        installed_packages: ["libjpeg-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            invok::packaging::PackageSpec::one("libjpeg-dev"),
            invok::packaging::PackageSpec::any(&["libjpeg8-dev", "libjpeg-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");

    let snap: invok::steps::apt_packages::AptDeltaSnapshot =
        serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile");

    assert_eq!(snap.already_installed, names(&["libjpeg-dev"]));
    assert!(
        snap.delta.is_empty(),
        "era già installato: non l'abbiamo aggiunto noi, non è nostro da rimuovere"
    );
}

/// and the run does not ask the manager to install one name twice.
#[test]
fn the_install_command_does_not_repeat_a_package() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        packages_without_candidate: ["libjpeg8-dev"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::with_specs(
        Box::new(ops),
        "install-system-dependencies",
        vec![
            invok::packaging::PackageSpec::one("libjpeg-dev"),
            invok::packaging::PackageSpec::any(&["libjpeg8-dev", "libjpeg-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.run(&ctx()).expect("run");

    let installs: Vec<Vec<String>> = ops_of(&log)
        .into_iter()
        .filter_map(|op| match op {
            Op::PkgInstall(pkgs) => Some(pkgs),
            _ => None,
        })
        .collect();

    assert_eq!(installs.len(), 1, "una sola invocazione");
    assert_eq!(installs[0], names(&["libjpeg-dev"]));
}

// --- the removal policy speaks to the manager, not to SystemOps -------------
//
// the recovery **sequence** is not checked here: `tests/apt_packages.rs`
// already guards it and passes **unchanged** after M1 — that invariance is the
// proof the extraction was neutral. rewriting it here would move the guarantee
// without adding to it.

/// an empty delta starts no command: there is nothing of ours to remove, and a
/// purge with no arguments would be noise.
#[test]
fn an_empty_delta_asks_the_manager_for_nothing() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        installed_packages: ["pippo"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::custom(
        Box::new(ops),
        "install-system-dependencies",
        names(&["pippo"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.undo(&ctx()).expect("undo");

    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::PkgRemove(_) | Op::PkgRepair)),
        "nessun pacchetto nel delta: il gestore non va nemmeno invocato"
    );
}

// --- production constructors are not the test ones --------------------------

/// **the regression clippy caught during M1**, written down.
///
/// two steps have constructors that are *not* interchangeable: one carries the
/// production timings (clone backoff, service settling wait), the other zeroes
/// them because tests must not sleep.
///
/// removing the old ones risked building the production sequence from the test
/// constructors: three **instant** clone attempts on a dead network are one
/// attempt, and the retry would be decorative. no mock test would notice —
/// mocks want the zero wait.
#[test]
fn the_production_sequence_uses_the_production_constructors() {
    let sorgente = std::fs::read_to_string("src/steps/mod.rs").expect("leggo steps/mod.rs");
    let inizio = sorgente
        .find("pub fn build_steps")
        .expect("build_steps esiste");
    let fine = sorgente[inizio..]
        .find("\n}\n")
        .map(|i| inizio + i)
        .expect("fine di build_steps");
    let corpo = &sorgente[inizio..fine];

    for step in ["CloneOdooRepo", "SetupSystemd"] {
        assert!(
            corpo.contains(&format!("{step}::for_run(")),
            "{step} ha parametri di produzione (attese, backoff) che i suoi test azzerano: \
             in `build_steps` va costruito con `for_run`, non con `with_ops`"
        );
    }
}

/// rehydration uses the zero-wait one, and rightly so: a step is being rebuilt
/// **to undo it**, and removing a directory or stopping a service needs no
/// waiting.
///
/// A-R8-1's distinction again: before reusing something for a new question, ask
/// which question it answered.
#[test]
fn the_rehydration_path_does_not_need_the_production_timings() {
    let make_ops = backend_factory(OsFamily::Debian).expect("backend Debian");
    for name in ["clone-odoo-repo", "setup-systemd"] {
        assert!(
            invok::steps::step_by_name(name, &make_ops).is_some(),
            "'{name}' dev'essere ricostruibile per l'undo"
        );
    }
}

// --- the boundary did not change the protection it guards -------------------

/// the undo purges **only** the delta, now that removal goes through the
/// manager too — checked from the side that matters, the commands that actually
/// reach the system.
#[test]
fn the_undo_still_removes_only_what_we_added() {
    let (ops, log) = MockSystemOps::new(MockConfig {
        installed_packages: ["gia-presente"]
            .iter()
            .map(|s| s.to_string())
            .collect::<HashSet<_>>(),
        ..MockConfig::default()
    });

    let mut step = AptPackagesStep::custom(
        Box::new(ops),
        "install-system-dependencies",
        names(&["gia-presente", "aggiunto-da-noi"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&ctx()).expect("snapshot");
    step.run(&ctx()).expect("run");
    step.undo(&ctx()).expect("undo");

    let purged: Vec<Vec<String>> = ops_of(&log)
        .into_iter()
        .filter_map(|op| match op {
            Op::PkgRemove(pkgs) => Some(pkgs),
            _ => None,
        })
        .collect();

    assert_eq!(
        purged,
        vec![names(&["aggiunto-da-noi"])],
        "il pacchetto che il cliente aveva già non si tocca, qualunque sia il gestore"
    );
}

// --- the availability decision, separated from the commands -----------------

/// the rule that protects A5.1-bis, checked **without apt at hand**.
///
/// the code that runs the queries needs a real machine; the *decision* that
/// follows does not, and without this separation it stayed out of every test —
/// as M1's mutation testing showed, where "call a virtual name real" survived
/// the whole suite.
#[test]
fn a_real_candidate_always_beats_a_virtual_name() {
    use invok::packaging::{availability_from, Availability};

    assert_eq!(availability_from(true, false), Availability::Real);
    assert_eq!(
        availability_from(false, true),
        Availability::VirtualOnly,
        "installabile ma non per questo nome: è un ripiego, e va detto"
    );
    assert_eq!(availability_from(false, false), Availability::Absent);
    assert_eq!(
        availability_from(true, true),
        Availability::Real,
        "se il candidato è reale la risposta è reale, qualunque cosa dica il \
         risolutore: un nome rimovibile batte sempre uno che non lo è"
    );
}
