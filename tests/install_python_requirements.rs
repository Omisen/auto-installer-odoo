//! [`InstallPythonRequirements`]: the no-op undo and the gevent workaround.

mod common;

use std::path::PathBuf;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::step::Step;
use invok::steps::install_python_requirements::{
    filter_out_gevent_stack, gevent_stack_lines, InstallPythonRequirements,
};

fn ctx() -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        dry_run: false,
        ..Default::default()
    }
}

const REQUIREMENTS: &str = "gevent==21.12.0 ; sys_platform != 'win32'\npytz\nBabel==2.9.1\n";

/// taken **verbatim** from Odoo's requirements file, its own comments included.
/// the fixture that matters: A-R6-3 was not a parsing slip but a failure to
/// notice there are **four** versions here, one per Python release — and Odoo
/// even annotates them.
const ODOO18_REQUIREMENTS: &str = "\
psycopg2==2.9.9\n\
gevent==21.8.0 ; sys_platform != 'win32' and python_version == '3.10'  # (Jammy)\n\
gevent==22.10.2; sys_platform != 'win32' and python_version > '3.10' and python_version < '3.12'\n\
gevent==24.2.1 ; sys_platform != 'win32' and python_version >= '3.12' and python_version < '3.13'  # (Noble)\n\
gevent==24.11.1 ; sys_platform != 'win32' and python_version >= '3.13'  # (Trixie)\n\
greenlet==1.1.2 ; sys_platform != 'win32' and python_version == '3.10'  # (Jammy)\n\
greenlet==2.0.2 ; sys_platform != 'win32' and python_version > '3.10' and python_version < '3.12'\n\
greenlet==3.0.3 ; sys_platform != 'win32' and python_version >= '3.12' and python_version < '3.13' # (Noble)\n\
Babel==2.9.1\n";

/// the arguments of the user-run operations, in order: the pip installs.
fn pip_calls(ops: &[Op]) -> Vec<Vec<String>> {
    ops.iter()
        .filter_map(|o| match o {
            Op::RunAsUser { program, args, .. } if program.ends_with("pip") => Some(args.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn undo_is_noop_pip_removal_belongs_to_venv() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    let after_run = ops_of(&log).len();

    step.undo(&c).expect("undo");
    let after_undo = ops_of(&log).len();

    // the undo executes NOTHING: no uninstall, no removal.
    assert_eq!(after_run, after_undo, "9c.undo must be a no-op");
    assert!(!ops_of(&log)
        .iter()
        .any(|o| matches!(o, Op::RemoveDirAll(_))));
}

#[test]
fn gevent_cython_workaround_sequence() {
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert_eq!(calls.len(), 4, "four pip steps were expected");

    // upgrade pip, wheel and setuptools.
    assert!(calls[0].contains(&"--upgrade".to_string()) && calls[0].contains(&"pip".to_string()));
    // a Cython the sources can build against.
    assert!(calls[1].contains(&"Cython<3".to_string()));
    // gevent without build isolation, from a requirements file.
    assert!(calls[2].contains(&"--no-build-isolation".to_string()));
    assert!(calls[2].contains(&"--requirement".to_string()));
    // the rest of the requirements, preferring binaries, without gevent.
    assert!(calls[3].contains(&"--prefer-binary".to_string()));
    assert!(calls[3].contains(&"--requirement".to_string()));
    assert!(!calls[3].iter().any(|a| a.contains("gevent")));
}

#[test]
fn setuptools_is_seeded_in_the_venv_before_the_no_build_isolation_step() {
    // A-R6-2: from Python 3.12 `venv` seeds only pip, and the isolated-build
    // step builds with what it finds IN THE VENV — without setuptools pip dies
    // on a missing build backend. the system package does not help: the venv is
    // isolated.
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert!(
        calls[0].iter().any(|a| a.starts_with("setuptools")),
        "the venv bootstrap must install setuptools: {:?}",
        calls[0]
    );

    // and it must come BEFORE that step, or it is pointless.
    let no_isolation = calls
        .iter()
        .position(|c| c.contains(&"--no-build-isolation".to_string()))
        .expect("the gevent step without isolation must exist");
    assert!(
        no_isolation > 0,
        "setuptools must be seeded before the build without isolation: {calls:?}"
    );
}

#[test]
fn the_venv_setuptools_is_bounded_below_the_release_that_dropped_pkg_resources() {
    // A-V3-26. setuptools 82.0.0 removed `pkg_resources`, which Odoo 16 imports
    // bare — so an unbounded `--upgrade setuptools` replaced a working venv
    // seed with one that makes `initialize-odoo-database` die. observed in the
    // field on Ubuntu 22.04, where `venv` seeds 59.6.0 and we upgraded it to
    // 84.
    //
    // the assertion is on the **bound**, not on the string: what must not come
    // back is asking pip for whatever exists today.
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    let spec = calls[0]
        .iter()
        .find(|a| a.starts_with("setuptools"))
        .unwrap_or_else(|| panic!("no setuptools in the venv bootstrap: {:?}", calls[0]));

    assert_ne!(
        spec, "setuptools",
        "setuptools must carry an upper bound, or pip takes whatever PyPI has today \
         — which since 82.0.0 has no pkg_resources"
    );

    let bound = spec
        .strip_prefix("setuptools<")
        .unwrap_or_else(|| panic!("expected an upper bound on setuptools, got {spec:?}"));
    let major: u32 = bound
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("unreadable setuptools bound: {bound:?}"));
    // two thresholds, asserted apart because they fail differently and a single
    // message would send the reader to the wrong one.
    assert!(
        major <= 82,
        "setuptools 82 removed pkg_resources: above this bound Odoo 16 dies at \
         initialize-odoo-database, which is the defect itself; got {spec:?}"
    );
    assert!(
        major <= 81,
        "setuptools 81 keeps pkg_resources but warns on every import — twice per Odoo 16 \
         start, in a branch that does not filter it and on a machine where nobody can \
         turn it off; got {spec:?}"
    );
}

#[test]
fn every_pip_call_caches_inside_our_perimeter() {
    // A-R5-3: without the flag pip writes into the `odoo` user's home, which is
    // pre-existing and never emptied by the rollback. the cache belongs inside
    // the venv, which the undo removes wholesale.
    let cfg = MockConfig {
        requirements_content: Some(REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert!(!calls.is_empty(), "the run must invoke pip");
    let expected = "/opt/odoo/odoo18/sandbox/.pip-cache".to_string();
    for (i, call) in calls.iter().enumerate() {
        let pos = call
            .iter()
            .position(|a| a == "--cache-dir")
            .unwrap_or_else(|| panic!("pip call #{i} must pass --cache-dir: {call:?}"));
        assert_eq!(
            call.get(pos + 1),
            Some(&expected),
            "pip cache #{i} must live inside the venv, not in the odoo home"
        );
    }
}

#[test]
fn missing_requirements_is_error() {
    let cfg = MockConfig {
        requirements_content: None, // requirements.txt assente
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    assert!(
        step.run(&c).is_err(),
        "a missing requirements file is an error"
    );
    assert!(
        pip_calls(&ops_of(&log)).is_empty(),
        "no install when the requirements file is missing"
    );
}

// --- A-R6-3: pip picks the gevent version -----------------------------------
//
// Odoo pins four gevents and five greenlets, one per Python version. taking
// "the first line starting with gevent" gave the same one on every system:
// right by coincidence on one release, and on another a version that does not
// compile. the fix is to stop choosing.

#[test]
fn every_pinned_version_survives_with_its_marker() {
    // the property that makes the fix a fix: pip's input holds EVERY version
    // with its marker. one left means somebody started choosing again.
    let lines = gevent_stack_lines(ODOO18_REQUIREMENTS);

    for version in ["21.8.0", "22.10.2", "24.2.1", "24.11.1"] {
        assert!(
            lines.contains(&format!("gevent=={version}")),
            "gevent=={version} is missing: pip can no longer choose. produced:\n{lines}"
        );
    }
    for version in ["1.1.2", "2.0.2", "3.0.3"] {
        assert!(
            lines.contains(&format!("greenlet=={version}")),
            "greenlet=={version} is missing: pip would resolve any version compatible with \
             gevent's metadata, which is how an old greenlet ended up compiling against a \
             newer Python. produced:\n{lines}"
        );
    }

    // and every line keeps its marker: the only thing separating the right
    // version from one that will not compile.
    for line in lines.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            line.contains("python_version"),
            "a line without an environment marker: pip would not know which to pick: {line}"
        );
    }
}

#[test]
fn the_complement_keeps_everything_else_and_nothing_of_the_stack() {
    let filtered = filter_out_gevent_stack(ODOO18_REQUIREMENTS);
    assert!(
        !filtered.to_lowercase().contains("gevent")
            && !filtered.to_lowercase().contains("greenlet"),
        "step 4 must not reinstall what step 3 already placed: {filtered}"
    );
    assert!(filtered.contains("psycopg2==2.9.9"));
    assert!(filtered.contains("Babel==2.9.1"));
}

#[test]
fn a_similarly_named_package_is_not_mistaken_for_the_stack() {
    // a differently named package is not gevent: the boundary after the name
    // exists for that, and without it the isolated step would drag it along.
    let requirements = "gevent-websocket==0.10.1\ngreenlet-stubs==1.0\ngevent==24.2.1 ; python_version >= '3.12'\n";
    let lines = gevent_stack_lines(requirements);
    assert!(lines.contains("gevent==24.2.1"));
    assert!(
        !lines.contains("gevent-websocket") && !lines.contains("greenlet-stubs"),
        "packages with a similar name were captured: {lines}"
    );
    assert!(filter_out_gevent_stack(requirements).contains("gevent-websocket==0.10.1"));
}

#[test]
fn requirements_without_gevent_produce_no_dedicated_step() {
    // no gevent means that step has no reason to exist: three pip calls instead
    // of four, and no pointless temporary.
    let cfg = MockConfig {
        requirements_content: Some("pytz\nBabel==2.9.1\n".to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    assert_eq!(calls.len(), 3, "no gevent step was expected: {calls:?}");
    assert!(
        gevent_stack_lines("pytz\nBabel==2.9.1\n").is_empty(),
        "without gevent the selection is empty, not an invented default"
    );
}

#[test]
fn pip_receives_a_file_never_a_hand_picked_version() {
    // the behavioural check, not just the pure function: no pip argument may be
    // a gevent version chosen by us.
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let calls = pip_calls(&ops_of(&log));
    let gevent_call = &calls[2];
    assert!(
        gevent_call.contains(&"--requirement".to_string()),
        "the gevent step must pass a file, so pip evaluates the markers: {gevent_call:?}"
    );
    assert!(
        !gevent_call.iter().any(|a| a.starts_with("gevent==")),
        "no version chosen by us on argv: {gevent_call:?}"
    );
    assert!(
        gevent_call.contains(&"--no-build-isolation".to_string()),
        "the Cython<3 workaround stays: on that release the pinned gevent has no wheel"
    );
}

// --- A-V3-3: where the temporary requirements are born ----------------------

/// the paths of files created through the fail-closed primitive.
fn created_private_files(ops: &[Op]) -> Vec<PathBuf> {
    ops.iter()
        .filter_map(|o| match o {
            Op::CreatePrivateFile(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

/// **A-V3-3's defect.** the two requirements files were born in a shared
/// directory under a name written in the source: root wrote them, pip read them
/// as another user, and anyone with a local account could replace them in the
/// window between and have arbitrary packages installed into the venv.
///
/// they are now born inside the venv's sandbox, owned by that user and not
/// writable by third parties: the attack's premise disappears rather than being
/// countered.
#[test]
fn requirements_are_written_inside_the_venv_not_in_a_shared_temp_dir() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let created = created_private_files(&ops_of(&log));
    assert_eq!(
        created.len(),
        2,
        "the two requirements files were expected (gevent and filtered): {created:?}"
    );

    let venv = c.install_dir.join("sandbox");
    for path in &created {
        assert!(
            path.starts_with(&venv),
            "{} must be born inside the venv and nowhere else: in a world-writable directory \
             a local user could replace it before pip reads it (A-V3-3)",
            path.display()
        );
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with('.') && name.ends_with(".tmp"),
            "the name is not unpredictable: {name}"
        );
    }
    assert_ne!(
        created[0], created[1],
        "two distinct files, not the same path"
    );
}

/// the file is born `0600 root` and pip reads it as another user, so without
/// the chown the step would fail. that is the only reason it exists.
#[test]
fn each_requirements_file_is_handed_over_to_the_odoo_user() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    for path in created_private_files(&ops) {
        assert!(
            ops.iter().any(|o| matches!(
                o,
                Op::ChownNamed { path: p, owner, group }
                    if *p == path && owner == "odoo" && group == "odoo"
            )),
            "{} is not handed to the odoo user: pip could not read it",
            path.display()
        );
    }
}

/// the temporaries are removed after use, through the same boundary that
/// created them. they live inside the venv anyway, so an interrupted run leaves
/// nothing outside the reversible perimeter.
#[test]
fn requirements_files_are_removed_after_use() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    for path in created_private_files(&ops) {
        assert!(
            ops.iter()
                .any(|o| matches!(o, Op::RemoveFile(p) if *p == path)),
            "{} is not removed after use",
            path.display()
        );
    }
}

// --- A-MD-7: when pip fails, say why ----------------------------------------

/// the gevent step's failure on a Python newer than Odoo's pins arrives with
/// the **cause in front** and the original error still behind it.
///
/// the last pin Odoo declares has no wheel for that interpreter, so pip
/// compiles and produces three hundred lines of `gcc` from which the real cause
/// cannot be recovered. that is this diagnosis's whole value: the reader must
/// see it is the **version**, not the build environment.
///
/// exercised through the step's `run` and not the pure function: a correct
/// diagnosis nobody invokes is indistinguishable from an absent one.
#[test]
fn a_gevent_failure_on_a_newer_python_says_why() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: Some((3, 14)),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("the gevent step failed")
        .to_string();

    assert!(
        err.contains("3.14"),
        "the diagnosis does not say which Python is underneath: {err}"
    );
    assert!(
        err.contains("3.13"),
        "the diagnosis does not say how far we get, so there is no telling how far ahead we are: {err}"
    );
    assert!(
        err.contains("gevent==24.11.1"),
        "the diagnosis does not show the pins Odoo declares: {err}"
    );
    assert!(
        err.contains("Building wheel for gevent"),
        "the original error is gone: explaining is not hiding the evidence: {err}"
    );
}

/// on a **covered** Python the same failure passes through untouched.
///
/// the half that makes the check a check: there the cause is something else,
/// and a wrong diagnosis is worse than none because it sends people to fix the
/// wrong thing (A-R9-1's lesson).
#[test]
fn on_a_covered_python_the_pip_error_is_left_alone() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: Some((3, 12)),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("the gevent step failed")
        .to_string();

    assert!(
        err.contains("Building wheel for gevent"),
        "pip's error must stay what it is: {err}"
    );
    assert!(
        !err.contains("newer than Python"),
        "on a covered Python A-MD-7's diagnosis is beside the point: {err}"
    );
}

/// and with an unknown Python, nothing is guessed.
///
/// `None` means "unknown", neither "fine" nor "too new": nothing is concluded
/// from absent information, and the command's own error stands.
#[test]
fn an_unknown_interpreter_does_not_become_a_guess() {
    let cfg = MockConfig {
        requirements_content: Some(ODOO18_REQUIREMENTS.to_string()),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        python_version: None,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = InstallPythonRequirements::with_ops(Box::new(mock));
    let c = ctx();

    step.snapshot(&c).expect("snapshot");
    let err = step
        .run(&c)
        .expect_err("the gevent step failed")
        .to_string();

    assert!(
        !err.contains("newer than Python"),
        "without knowing the version there is no claiming it is too new: {err}"
    );
}
