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
    let plan = choose_python(Some((3, 12)), &fedora_alternates(), &system_dev(), "18");
    assert_eq!(plan, PythonPlan::default());
    assert!(plan.is_system());
    assert_eq!(plan.command, "python3");

    // exactly on the threshold too: "exercised" means installations reach the
    // end there, so there is nothing to replace.
    let plan = choose_python(
        Some(NEWEST_TESTED_PYTHON),
        &fedora_alternates(),
        &system_dev(),
        "18",
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
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");

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
    let plan = choose_python(Some((3, 14)), &troppo_nuovi, &system_dev(), "18");
    assert!(
        plan.is_system(),
        "no covered interpreter: we stay where we are"
    );
    assert!(plan.packages.is_empty());
}

/// with no packaged alternative, we stay on the system interpreter.
#[test]
fn without_alternates_there_is_nothing_to_choose() {
    let plan = choose_python(Some((3, 14)), &[], &system_dev(), "18");
    assert!(plan.is_system());
}

/// "I do not know which Python is there" is not "it is too new".
///
/// nothing is concluded from absent information, least of all installing a
/// second interpreter on a customer's machine.
#[test]
fn an_unknown_system_interpreter_does_not_trigger_an_installation() {
    let plan = choose_python(None, &fedora_alternates(), &system_dev(), "18");
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
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");
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
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");
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
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");
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
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");
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
            .any(|alt| !python_is_newer_than_tested(alt.version, "18")),
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

// --- A-V3-29: the ceiling depends on WHICH Odoo -----------------------------
//
// what an installation completes is a **pair**, `Odoo version × Python`. Until
// now the ceiling was one number for every branch, and that silently claimed
// they share one — they do not. `A-V3-28` had already named the flaw while
// fixing only the message: *"what breaks the build is «does this Odoo pin a
// wheel for this interpreter?», and the version is not an input of that
// constant"*. Here it is.
//
// the numbers are **read from Odoo's `requirements.txt`**, not inferred: every
// branch pins gevent in brackets keyed on `python_version`, and what decides is
// whether the newest bracket has an upper bound. 17/18/19 stop at `>= '3.13'`
// with a release that has cp313 wheels; **16 does not stop at all** — it pins
// `24.2.1` for `>= '3.12'` and nothing above, so past 3.12 pip keeps choosing a
// release whose newest wheel is cp312.

/// Odoo 16 is capped at 3.12; every other branch keeps 3.13.
#[test]
fn the_ceiling_is_lower_for_odoo_16_and_unchanged_for_the_rest() {
    use invok::checks::newest_tested_python;

    assert_eq!(
        newest_tested_python("16"),
        (3, 12),
        "16's newest gevent bracket is unbounded and its wheels stop at cp312"
    );
    for other in ["17", "18", "19"] {
        assert_eq!(
            newest_tested_python(other),
            NEWEST_TESTED_PYTHON,
            "{other} pins gevent 24.11.1 for >= 3.13, which ships a cp313 wheel"
        );
    }
}

/// the case the whole voice exists for: Odoo 16 on a Fedora whose system Python
/// is 3.13.
///
/// before this, 3.13 was "covered" — the single ceiling said so — and the
/// installation went ahead on it and died building gevent. Now the same machine
/// builds the venv on 3.12, which the catalogue has already offered since
/// `A-MD-7`: the machinery to install and undo an alternative interpreter was
/// there, only the decision did not know which Odoo it was deciding for.
#[test]
fn odoo_16_on_a_313_system_moves_to_312() {
    let plan = choose_python(Some((3, 13)), &fedora_alternates(), &system_dev(), "16");

    assert_eq!(plan.command, "python3.12");
    assert!(!plan.is_system());
    assert!(
        plan.packages.contains(&"python3.12".to_string())
            && plan.packages.contains(&"python3.12-devel".to_string()),
        "the interpreter and its headers are added to the delta, so the undo takes them back: {:?}",
        plan.packages
    );

    // and the very same machine with Odoo 18 keeps its system interpreter:
    // the change has to be invisible where it is not needed.
    let plan18 = choose_python(Some((3, 13)), &fedora_alternates(), &system_dev(), "18");
    assert!(
        plan18.is_system(),
        "3.13 is covered for 18, so nothing is installed"
    );
}

/// on a 3.14 system the two branches part company: 16 goes to 3.12, 18 to 3.13.
///
/// this is the assertion that would fail if the ceiling went back to being one
/// number — whichever number it was.
#[test]
fn on_a_314_system_each_branch_lands_on_its_own_ceiling() {
    let sixteen = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "16");
    let eighteen = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev(), "18");

    assert_eq!(sixteen.command, "python3.12");
    assert_eq!(eighteen.command, "python3.13");
    assert_ne!(
        sixteen.command, eighteen.command,
        "one ceiling for every branch is exactly what A-V3-29 removed"
    );
}

/// no interpreter low enough: carry on with the system one.
///
/// a refusal without evidence blocks the good case (A5.1-bis), and the failure
/// that follows is now explained rather than left as a wall of `gcc`.
#[test]
fn odoo_16_with_no_low_enough_interpreter_falls_back_to_the_system_one() {
    let only_313 = vec![AlternatePython::new(
        (3, 13),
        "python3.13",
        "python3.13-devel",
    )];
    let plan = choose_python(Some((3, 14)), &only_313, &system_dev(), "16");

    assert!(
        plan.is_system(),
        "3.13 is above 16's ceiling, so it is not an answer either: {plan:?}"
    );
}

/// the warning cites the ceiling **of this Odoo**, not a number from elsewhere.
///
/// on Fedora 41 with Odoo 16 the old message could not fire at all: 3.13 was
/// not "newer than tested", because tested was 3.13. The silence was the bug.
#[test]
fn the_warning_speaks_for_the_branch_being_installed() {
    use invok::checks::untested_python_warning;

    let for_16 = untested_python_warning((3, 13), "16")
        .expect("3.13 is above 16's ceiling and must be reported");
    assert!(
        for_16.contains("3.12"),
        "it must cite the ceiling that applies here: {for_16}"
    );
    assert_eq!(
        untested_python_warning((3, 13), "18"),
        None,
        "and stay silent where 3.13 is covered, or it cries wolf"
    );
}
