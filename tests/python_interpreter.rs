//! M11: **which interpreter** builds the virtualenv, and what follows.
//!
//! the defect this closes (A-MD-7) is not in our code but in Odoo's pins: on a
//! newer system Python the pinned `gevent` has no wheel and its generated C
//! does not compile. M10 *says* so; M11 makes it not happen, by building the
//! venv on an interpreter those pins cover.
//!
//! verified in the field before a line was written: on the alternative
//! interpreter the whole requirements file installs, `gevent` included and as a
//! prebuilt wheel.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::checks::{choose_python, PythonPlan, NEWEST_TESTED_PYTHON};
use invok::context::Context;
use invok::packaging::{AlternatePython, PackageSpec};
use invok::step::Step;
use invok::steps::apt_packages::AptPackagesStep;
use invok::steps::create_virtualenv::CreateVirtualenv;
use std::path::PathBuf;

fn fedora_alternates() -> Vec<AlternatePython> {
    vec![
        AlternatePython::new((3, 13), "python3.13", "python3.13-devel"),
        AlternatePython::new((3, 12), "python3.12", "python3.12-devel"),
    ]
}

fn system_dev() -> Vec<String> {
    vec!["python3-devel".to_string()]
}

// --- the choice -------------------------------------------------------------

/// a Python covered by the pins is left alone: no extra interpreter, nothing
/// added to the delta.
///
/// the half that makes M11 **invisible** where it is not needed. a phase that
/// changed behaviour there too would have been far riskier than the one needed.
#[test]
fn a_supported_system_interpreter_is_left_alone() {
    let plan = choose_python(Some((3, 12)), &fedora_alternates(), &system_dev());
    assert_eq!(plan, PythonPlan::default());
    assert!(plan.is_system());
    assert_eq!(plan.command, "python3");

    // exactly on the threshold too: "exercised" means installations reach the
    // end there, so there is nothing to replace.
    let plan = choose_python(
        Some(NEWEST_TESTED_PYTHON),
        &fedora_alternates(),
        &system_dev(),
    );
    assert!(plan.is_system(), "on the tested version nothing changes");
}

/// a Python newer than the pins builds the venv on the newest **covered**
/// interpreter, not the oldest available.
///
/// the direction matters: one closer to the system's gets security updates
/// longer, while staying inside what the installer really exercises.
#[test]
fn an_unsupported_system_interpreter_is_replaced_by_the_newest_covered_one() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());

    assert_eq!(plan.command, "python3.13", "3.13 beats 3.12: it is newer");
    assert!(!plan.is_system());
    assert_eq!(
        plan.packages,
        vec!["python3.13".to_string(), "python3.13-devel".to_string()],
        "the interpreter alone is not enough: without headers no extension compiles"
    );
    assert_eq!(
        plan.supersedes,
        system_dev(),
        "the system Python's headers are of no use to anyone now"
    );
}

/// if the only alternatives are themselves newer than the pins, nothing is
/// installed: we stay on the system one and the warning speaks.
///
/// the branch that stops the choice becoming "take something anyway":
/// installing a second, equally uncovered interpreter would mutate a customer's
/// machine for nothing.
#[test]
fn an_alternate_that_is_just_as_new_is_not_a_solution() {
    let troppo_nuovi = vec![AlternatePython::new(
        (3, 15),
        "python3.15",
        "python3.15-devel",
    )];
    let plan = choose_python(Some((3, 14)), &troppo_nuovi, &system_dev());
    assert!(
        plan.is_system(),
        "no covered interpreter: we stay where we are"
    );
    assert!(plan.packages.is_empty());
}

/// with no packaged alternative, we stay on the system interpreter.
#[test]
fn without_alternates_there_is_nothing_to_choose() {
    let plan = choose_python(Some((3, 14)), &[], &system_dev());
    assert!(plan.is_system());
}

/// "I do not know which Python is there" is not "it is too new".
///
/// nothing is concluded from absent information, least of all installing a
/// second interpreter on a customer's machine.
#[test]
fn an_unknown_system_interpreter_does_not_trigger_an_installation() {
    let plan = choose_python(None, &fedora_alternates(), &system_dev());
    assert!(plan.is_system());
    assert!(plan.packages.is_empty());
}

// --- consequences for the package list --------------------------------------

fn specs(names: &[&str]) -> Vec<PackageSpec> {
    names.iter().map(|n| PackageSpec::one(n)).collect()
}

/// with the system interpreter the package list is **unchanged**.
#[test]
fn the_package_list_is_untouched_when_the_system_interpreter_is_used() {
    let list_ = specs(&["python3-devel", "gcc", "libpq-devel"]);
    assert_eq!(PythonPlan::default().adapt_specs(&list_), list_);
}

/// with an alternative one: the system headers out, its own in.
///
/// the rest of the list is untouched — the compiler and the client headers are
/// still needed, and the extensions pip builds are the same.
#[test]
fn the_alternate_interpreter_replaces_the_system_headers_and_nothing_else() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let adattata = plan.adapt_specs(&specs(&["python3-devel", "gcc", "libpq-devel"]));
    let names: Vec<&str> = adattata.iter().map(|s| s.preferred()).collect();

    assert!(
        !names.contains(&"python3-devel"),
        "the system Python's headers are of no use to a venv on 3.13: {names:?}"
    );
    assert!(
        names.contains(&"python3.13"),
        "manca l'interprete: {names:?}"
    );
    assert!(
        names.contains(&"python3.13-devel"),
        "its headers are missing, and six C extensions would not compile: {names:?}"
    );
    assert!(
        names.contains(&"gcc") && names.contains(&"libpq-devel"),
        "the rest of the list is unrelated and must stay: {names:?}"
    );
}

// --- consequences for the steps ---------------------------------------------

fn ctx_with(plan: PythonPlan) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        python: plan,
        ..Default::default()
    }
}

fn installed(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter_map(|o| match o {
            Op::PkgInstall(pkgs) => Some(pkgs.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// the interpreter is installed by the step whose undo **purges the delta**.
///
/// not the bootstrap one, whose undo leaves what it added installed: a 43 MB
/// interpreter put there by us and never removed would be a leftover inside the
/// perimeter the rollback promises to restore.
#[test]
fn the_interpreter_is_installed_by_the_step_whose_undo_purges_it() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let ctx = ctx_with(plan);

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    deps.snapshot(&ctx).expect("snapshot");
    deps.run(&ctx).expect("run");
    let packages = installed(&ops_of(&log));
    assert!(
        packages.iter().any(|p| p == "python3.13"),
        "install-system-dependencies must carry the interpreter: {packages:?}"
    );

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut boot = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    boot.snapshot(&ctx).expect("snapshot");
    boot.run(&ctx).expect("run");
    let packages = installed(&ops_of(&log));
    assert!(
        !packages.iter().any(|p| p.starts_with("python3.1")),
        "bootstrap must NOT carry it: its undo would not remove it ({packages:?})"
    );
}

/// the venv is built on the chosen interpreter, and the precondition questions
/// **that one**.
///
/// the same question asked twice must have the same answer: asking one
/// interpreter and then building with another would again be a check that talks
/// about something else (A-R6-1).
#[test]
fn the_virtualenv_is_born_on_the_chosen_interpreter() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let ctx = ctx_with(plan);

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    step.snapshot(&ctx).expect("snapshot");
    step.run(&ctx).expect("run");

    assert!(
        ops_of(&log).iter().any(|o| matches!(
            o,
            Op::CreateVenv { python, .. } if python == "python3.13"
        )),
        "the venv must be born on python3.13: {:?}",
        ops_of(&log)
    );
}

/// A-MD-7's diagnosis questions the **venv's** interpreter, not the system's.
///
/// the two can now diverge, and naming the wrong one would send the reader
/// looking for a cause that is not there. the same shape of defect M11
/// corrects, one level further — and without recording *which* name is asked,
/// no test could see it.
#[test]
fn the_failure_diagnosis_asks_the_interpreter_the_venv_actually_uses() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let mut ctx = ctx_with(plan);
    ctx.dry_run = false;

    let cfg = MockConfig {
        requirements_content: Some(
            "gevent==24.11.1 ; sys_platform != 'win32' and python_version >= '3.13'\n".to_string(),
        ),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = invok::steps::install_python_requirements::InstallPythonRequirements::with_ops(
        Box::new(mock),
    );
    step.snapshot(&ctx).expect("snapshot");
    let _ = step.run(&ctx).expect_err("the gevent step failed");

    assert!(
        ops_of(&log)
            .iter()
            .any(|o| matches!(o, Op::PythonVersion(name) if name == "python3.13")),
        "the diagnosis must ask python3.13 for the version, not python3: {:?}",
        ops_of(&log)
    );
}

/// the family must offer **at least one** interpreter covered by the pins, or
/// M11 silently does nothing there.
///
/// the ritual question applied to a table rather than a check: in production,
/// can this list lead to a choice other than "stay on the system one"? if the
/// constant rose or the list emptied, the code would keep working and install
/// nothing — with no red to say so.
#[test]
fn fedora_offers_at_least_one_interpreter_covered_by_the_pins() {
    use invok::checks::python_is_newer_than_tested;
    use invok::packaging::dnf::DnfBackend;
    use invok::packaging::PackageManager;

    let catalog = DnfBackend.catalog();
    assert!(
        !catalog.alternate_pythons.is_empty(),
        "without alternatives, M11 on this family is dead code"
    );
    assert!(
        catalog
            .alternate_pythons
            .iter()
            .any(|alt| !python_is_newer_than_tested(alt.version)),
        "none of the alternatives is covered by the pins: the choice would always fall \
         back on the system interpreter"
    );
    // and every alternative carries its headers: the interpreter alone builds
    // nothing.
    for alt in &catalog.alternate_pythons {
        assert!(
            alt.devel.starts_with(&alt.interpreter),
            "{} does not carry the matching headers ({})",
            alt.interpreter,
            alt.devel
        );
    }
}
