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

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // only the completed ones are undone, in reverse order.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
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

    assert!(result.is_err(), "l'esecuzione deve fallire su gamma");
    let order = log.lock().expect("lock").clone();
    // it acts and fails, and the other is cleaned up regardless.
    assert_eq!(order, vec!["beta".to_string(), "alpha".to_string()]);
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

    assert!(result.is_err(), "l'esecuzione deve fallire su beta");
    // the undo was invoked…
    assert_eq!(
        alpha_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "undo di alpha deve essere invocato dal motore"
    );
    // …but performed no action: an empty log means no artifact of ours.
    assert!(
        log.lock().expect("lock").is_empty(),
        "un undo su Preexisting non deve compiere azioni"
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
    assert_eq!(mode & 0o777, 0o600, "il file di stato deve essere 0600");

    // the records survive the round trip.
    let reloaded = InstallState::load(&path).expect("load");
    assert_eq!(reloaded, state);
    assert_eq!(reloaded.completed.len(), 2);
    assert_eq!(reloaded.completed[0].name, "alpha");
    assert_eq!(reloaded.completed[1].name, "beta");

    // a missing file yields an empty state, not an error.
    InstallState::clear(&path).expect("clear");
    let empty = InstallState::load(&path).expect("load assente");
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
        .expect_err("un'interruzione deve fermare l'esecuzione");
    assert!(
        err.to_string().contains("interrupted"),
        "il messaggio deve dire cosa è successo: {err}"
    );

    // the next step never started, and the previous two were undone in reverse
    // order — exactly as on a failure.
    let azioni = log.lock().expect("log").clone();
    assert_eq!(
        azioni,
        vec!["beta".to_string(), "alpha".to_string()],
        "gli step già eseguiti vanno annullati dall'ultimo al primo: {azioni:?}"
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
    let azioni = log.lock().expect("log").clone();
    assert_eq!(
        azioni,
        vec!["alpha".to_string()],
        "lo step in corso va completato (e quindi annullato); beta non è mai partito"
    );
    assert!(
        installer.state().completed.is_empty(),
        "annullato alpha, sul sistema non resta nulla: il manifesto deve dirlo"
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
        .expect("nessuna interruzione: l'esecuzione arriva in fondo");

    // and with a flag nobody raises.
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];
    Installer::new()
        .watching_interrupt(Arc::new(AtomicBool::new(false)))
        .execute(&mut steps, &ctx)
        .expect("flag mai alzato: nessun effetto");
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
        "alpha è stato annullato: il manifesto non deve più elencarlo, trovato {:?}",
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
        .expect("il secondo giro deve arrivare in fondo");

    assert_eq!(
        esecuzioni.load(Ordering::SeqCst),
        1,
        "alpha era stato annullato: saltarlo lascerebbe l'installazione a costruire \
         su artefatti che non esistono"
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

    let stato = InstallState::load(&ctx.state_path).expect("load");
    let nomi: Vec<&str> = stato.completed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        nomi,
        vec!["alpha"],
        "l'undo di alpha è fallito: il residuo resta registrato"
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
        "annullato tutto, il manifesto non descrive più niente: va rimosso, non svuotato"
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
        "c'è un residuo: il manifesto resta"
    );
    let stato = InstallState::load(&ctx.state_path).expect("load");
    assert_eq!(
        stato
            .completed
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
}
