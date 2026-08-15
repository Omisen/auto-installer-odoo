//! the engine end to end, against the four invariants of `CLAUDE.md`.
//!
//! nothing touches the system: `NoopStep` plus a tempdir for the state file, so
//! everything runs unprivileged.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use invok::context::Context;
use invok::engine::Installer;
use invok::state::{InstallState, PreState, StepRecord};
use invok::step::Step;
use invok::steps::noop::{NoopStep, UndoLog};

/// a non-dry context with its state file in a tempdir. the engine only uses the
/// dry-run flag and that path; the rest is irrelevant here.
fn ctx_with_state(dir: &tempfile::TempDir) -> Context {
    Context {
        dry_run: false,
        ..Default::default()
    }
    .with_state_path(dir.path().join(".installer-state.json"))
}

/// invariant 2: the rollback undoes from the last completed step to the first.
#[test]
fn rollback_runs_undo_in_reverse_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // the first two complete; the third fails and triggers the rollback.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(NoopStep::new("beta").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("gamma")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "the run must fail on gamma");
    let order = log.lock().expect("lock").clone();
    // reverse order, and the **failing** step goes first (A-V3-24): a `run`
    // that stops halfway has usually already created something, and leaving it
    // out of the rollback leaves that on disk.
    assert_eq!(
        order,
        vec!["gamma".to_string(), "beta".to_string(), "alpha".to_string()]
    );
}

/// invariant 3: a failing undo does not stop the others.
#[test]
fn rollback_is_best_effort() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // the second's undo fails; the first must still be cleaned up after it.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("beta")
                .fail_on_undo()
                .with_undo_log(Arc::clone(&log)),
        ),
        Box::new(
            NoopStep::new("gamma")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "the run must fail on gamma");
    let order = log.lock().expect("lock").clone();
    // it acts and fails, and the others are cleaned up regardless — gamma
    // included, being the step that failed (A-V3-24).
    assert_eq!(
        order,
        vec!["gamma".to_string(), "beta".to_string(), "alpha".to_string()]
    );
}

/// invariant 3 and the protection: a `Preexisting` step performs no undo
/// action, even though the engine invokes its undo.
#[test]
fn preexisting_step_undo_is_noop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // pre-existing, so not ours: the undo must be a no-op.
    let alpha = NoopStep::new("alpha")
        .preexisting()
        .with_undo_log(Arc::clone(&log));
    let alpha_calls = alpha.undo_call_handle();

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(alpha),
        Box::new(
            NoopStep::new("beta")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::new();
    let result = installer.execute(&mut steps, &ctx);

    assert!(result.is_err(), "the run must fail on beta");
    // the undo was invoked…
    assert_eq!(
        alpha_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "alpha's undo must be invoked by the engine"
    );
    // …but performed no action. the log is not empty — it holds `beta`, the
    // failing step, which A-V3-24 now undoes as well — so the assertion is
    // about alpha and only alpha.
    assert!(
        !log.lock().expect("lock").contains(&"alpha".to_string()),
        "an undo on Preexisting must take no action"
    );
}

/// invariant 4: the state survives a write/read round trip, `0600`.
#[test]
fn install_state_roundtrip_and_permissions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".installer-state.json");

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });
    state.record(StepRecord {
        name: "beta".to_string(),
        snapshot: serde_json::to_value(PreState::Preexisting).expect("serialize"),
    });

    state.save(&path).expect("save");

    // permissions.
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "the state file must be 0600");

    // the records survive the round trip.
    let reloaded = InstallState::load(&path).expect("load");
    assert_eq!(reloaded, state);
    assert_eq!(reloaded.completed.len(), 2);
    assert_eq!(reloaded.completed[0].name, "alpha");
    assert_eq!(reloaded.completed[1].name, "beta");

    // a missing file yields an empty state, not an error.
    InstallState::clear(&path).expect("clear");
    let empty = InstallState::load(&path).expect("load with nothing there");
    assert!(empty.completed.is_empty());
}

/// the happy path: every step completes and the state is persisted.
#[test]
fn successful_run_persists_all_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::new();
    installer.execute(&mut steps, &ctx).expect("esecuzione ok");

    assert_eq!(installer.state().completed.len(), 2);

    let persisted = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(persisted.completed.len(), 2);
    assert_eq!(persisted.completed[0].name, "alpha");
    assert_eq!(persisted.completed[1].name, "beta");
}

// --- B-V3-5: interruption from outside --------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

/// **the defect it closes.** a Ctrl-C killed the process outright, so the
/// in-process rollback never ran and the system was left half-done. the
/// interruption is now a request the engine watches, and the completed steps
/// are undone as if a step had failed.
#[test]
fn an_interrupt_rolls_back_what_was_already_done() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));
    let interrupted = Arc::new(AtomicBool::new(false));

    // the flag is raised mid-step, which is what a Ctrl-C does.
    let flag_per_beta = Arc::clone(&interrupted);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").with_undo_log(Arc::clone(&log))),
        Box::new(
            NoopStep::new("beta")
                .with_undo_log(Arc::clone(&log))
                .on_run(move || flag_per_beta.store(true, Ordering::SeqCst)),
        ),
        Box::new(NoopStep::new("gamma").with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new().watching_interrupt(Arc::clone(&interrupted));
    let err = installer
        .execute(&mut steps, &ctx)
        .expect_err("an interruption must stop the run");
    assert!(
        err.to_string().contains("interrupted"),
        "the message must say what happened: {err}"
    );

    // the next step never started, and the previous two were undone in reverse
    // order — exactly as on a failure.
    let actions = log.lock().expect("log").clone();
    assert_eq!(
        actions,
        vec!["beta".to_string(), "alpha".to_string()],
        "the steps already run are undone last to first: {actions:?}"
    );
}

/// the step in flight is **carried to completion**: stopping it halfway would
/// leave the package manager inconsistent or a database half-initialised. the
/// safe boundary is the one the engine already knows.
#[test]
fn the_step_in_flight_is_allowed_to_finish() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let interrupted = Arc::new(AtomicBool::new(false));
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    let flag = Arc::clone(&interrupted);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(
            NoopStep::new("alpha")
                .with_undo_log(Arc::clone(&log))
                .on_run(move || flag.store(true, Ordering::SeqCst)),
        ),
        Box::new(NoopStep::new("beta").with_undo_log(Arc::clone(&log))),
    ];

    let mut installer = Installer::new().watching_interrupt(Arc::clone(&interrupted));
    let _ = installer.execute(&mut steps, &ctx);

    // the proof it completed is that the rollback **undoes** it: an undo runs
    // only on a completed step. not sought in the manifest, where — rightly —
    // it is gone afterwards: the manifest says what remains, not what was done.
    let actions = log.lock().expect("log").clone();
    assert_eq!(
        actions,
        vec!["alpha".to_string()],
        "the step in progress is completed, and therefore undone; beta never started"
    );
    assert!(
        installer.state().completed.is_empty(),
        "with alpha undone nothing is left on the system: the manifest must say so"
    );
}

/// without an interruption nothing changes: the default flag is never raised,
/// and wiring one is optional.
#[test]
fn without_an_interrupt_nothing_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    // without the flag wired.
    Installer::new()
        .execute(&mut steps, &ctx)
        .expect("no interruption: the run reaches the end");

    // and with a flag nobody raises.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];
    Installer::new()
        .watching_interrupt(Arc::new(AtomicBool::new(false)))
        .execute(&mut steps, &ctx)
        .expect("the flag was never raised: no effect");
}

/// **A-R8-1.** after a failure the automatic rollback undoes the completed
/// steps — and the manifest **must stop listing them**.
///
/// the documented flow is "if it fails, fix the cause and run it again". with
/// the resume introduced in R8, a manifest still listing undone steps made the
/// re-run skip exactly those: the installation carried on assuming the home,
/// the user and the database the rollback had just removed.
///
/// the rule that closes it: **the manifest says what is still on the system**,
/// not what was done at some point.
#[test]
fn a_rolled_back_step_is_re_executed_on_the_next_run() {
    use std::sync::atomic::AtomicUsize;

    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    // first pass: one succeeds, the next fails, and the rollback undoes it.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    let dopo_rollback = InstallState::load(&ctx.state_path).expect("load");
    assert!(
        dopo_rollback.completed.is_empty(),
        "alpha was undone: the manifest must no longer list it, found {:?}",
        dopo_rollback
            .completed
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
    );

    // second pass: it must be RE-RUN, not skipped.
    let esecuzioni = Arc::new(AtomicUsize::new(0));
    let contatore = Arc::clone(&esecuzioni);
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").on_run(move || {
            contatore.fetch_add(1, Ordering::SeqCst);
        })),
        Box::new(NoopStep::new("beta")),
    ];
    Installer::resuming_from(dopo_rollback)
        .execute(&mut steps, &ctx)
        .expect("the second round must reach the end");

    assert_eq!(
        esecuzioni.load(Ordering::SeqCst),
        1,
        "alpha had been undone: skipping it would leave the installation building on \
         artifacts that do not exist"
    );
}

/// but a **failed** undo keeps its record: there the artifact may still be on
/// the system, and that record is the only trace of the leftover to retry.
/// forgetting it would lose the information exactly where it is needed.
#[test]
fn a_failed_undo_keeps_its_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_undo()),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    let state = InstallState::load(&ctx.state_path).expect("load");
    let names: Vec<&str> = state.completed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["alpha"],
        "alpha's undo failed: the leftover stays recorded"
    );
}

/// with **nothing** left after the rollback, the manifest must go too.
///
/// a file describing zero artifacts is a leftover that lies: it would make
/// `invok rollback` believe there is something to consume, and would stay on
/// disk indefinitely. found by the CI, which rightly asserted the manifest had
/// been consumed.
#[test]
fn an_empty_manifest_is_removed_not_left_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    assert!(
        !ctx.state_path.exists(),
        "with everything undone the manifest describes nothing: it is removed, not emptied"
    );
}

/// but if something remains — a failed undo — the manifest **must** stay: it is
/// the only trace of the leftover to retry.
#[test]
fn a_manifest_with_residue_stays_on_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_undo()),
        Box::new(NoopStep::new("beta").fail_on_run()),
    ];
    let _ = Installer::new().execute(&mut steps, &ctx);

    assert!(
        ctx.state_path.exists(),
        "there is a leftover: the manifest stays"
    );
    let state = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(
        state
            .completed
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
}

/// acting on an interruption belongs to the **engine**, between one step and
/// the next — and nowhere else.
///
/// this guard exists because the rule was broken while fixing something else:
/// `A-V3-22` put the network commands in a process group of their own, which
/// stopped Ctrl-C from reaching them, and the wait was taught to abort on the
/// flag to make up for it. it looked like responsiveness. it meant the step
/// *failed*, and a failed step is not in `completed`, so its undo never runs
/// and what it had already created stays on disk (`A-V3-24`). the CI job that
/// interrupts a real installation went from a clean system to a `/opt/odoo`
/// left behind.
///
/// reading the flag elsewhere is fine — to log, to explain a wait. **Returning**
/// the interruption error from anywhere but the engine is what turns a pause
/// into a half-done step.
#[test]
fn only_the_engine_turns_an_interruption_into_an_error() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();

    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            // the definition, and the one legitimate caller.
            if name == "interrupt.rs" || name == "engine.rs" {
                continue;
            }
            let content = std::fs::read_to_string(&path).expect("read");
            for (n, line) in content.lines().enumerate() {
                // the call, not a mention of it in a comment.
                if line.contains("interrupted_error()") && !line.trim_start().starts_with("//") {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "an interruption becomes an error outside the engine, which makes the step in progress \
         fail instead of finishing — and a failed step is never undone:\n{}",
        offenders.join("\n")
    );
}

/// `A-V3-24`: a step that fails **after** creating something is undone too.
///
/// found in the field, and by the CI job that interrupts a real installation:
/// `clone-odoo-repo` makes its directories before going to the network, so an
/// interrupted clone left `/opt/odoo/odoo18/repos/modules` behind — which made
/// `install_dir` non-empty, which made `/opt/odoo` non-empty, which the last
/// undo correctly refuses to remove. the dominant promise, defeated by a step
/// that was never asked to clean up after itself.
///
/// and it poisoned every later run: the next `prepare-opt-root` finds the
/// directory `Preexisting`, so its undo is a legitimate no-op forever.
///
/// the counter-proof matters as much: a step that fails **before** creating
/// anything must still undo nothing.
#[test]
fn the_step_that_fails_is_undone_too_but_only_if_it_created_something() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    // it created something, then failed: its undo must run.
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));
    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(
        NoopStep::new("half-done")
            .fail_on_run()
            .with_undo_log(Arc::clone(&log)),
    )];
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());
    assert_eq!(
        log.lock().expect("lock").clone(),
        vec!["half-done".to_string()],
        "what it had already created has to come off"
    );

    // it created nothing — `Preexisting` stands for "not ours" — so the undo
    // is invoked and does nothing. the gate stays the `PreState`, never the
    // fact that the step failed.
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));
    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(
        NoopStep::new("nothing-done")
            .preexisting()
            .fail_on_run()
            .with_undo_log(Arc::clone(&log)),
    )];
    let mut installer = Installer::new();
    assert!(installer.execute(&mut steps, &ctx).is_err());
    assert!(
        log.lock().expect("lock").is_empty(),
        "a step that created nothing must destroy nothing"
    );
}

/// the ordering rule `A-V3-24` rests on, for every step that creates something
/// and then tidies it up: **the promotion to `CreatedByUs` comes before the
/// ownership calls, never after.**
///
/// two behavioural tests prove what this means — a system user
/// (`tests/create_odoo_user.rs`) and a line in the customer's `.bashrc`
/// (`tests/patch_bashrc.rs`) both survive a failure that lands between the
/// creation and the `chmod`. this one extends the same rule to the steps whose
/// residue would be milder, without five more near-identical tests: the grep
/// sees the shape, the two above see the consequence.
#[test]
fn every_run_claims_ownership_before_tidying_up() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/steps");
    let ownership = ["self.ops.chmod(", ".chown_named(", ".chown_to_user("];
    let mut offenders: Vec<String> = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("read");
        // the `run` body, which is where the order matters: a `snapshot` never
        // mutates and an `undo` restores rather than creates.
        let Some(start) = content.find("fn run(&mut self") else {
            continue;
        };
        let body = &content[start..];
        let end = body.find("\n    fn ").unwrap_or(body.len());
        let body = &body[..end];

        let promotion = body.find("= PreState::CreatedByUs");
        let tidy = ownership.iter().filter_map(|c| body.find(c)).min();
        if let (Some(promotion), Some(tidy)) = (promotion, tidy) {
            if tidy < promotion {
                offenders.push(format!(
                    "{}: an ownership call comes before the step claims what it created",
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "a failure between the creation and the tidying leaves an artifact no undo will \
         ever touch — the step is not in `completed`, and its `PreState` still says \
         `Untracked`:\n{}",
        offenders.join("\n")
    );
}
