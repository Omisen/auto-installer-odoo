//! A-V3-1: a manifest already on disk is never silently overwritten.
//!
//! two levels, both unprivileged: the **policy** — install, resume or refuse —
//! which is a pure function, and the **engine's resume**, i.e. what really
//! happens to steps that already ran.
//!
//! the defect these guard was not in a step but in `main`, between pieces each
//! covered on its own. that is why the rule moved into the library: so a test
//! could reach it.

use std::sync::{Arc, Mutex};

use invok::context::Context;
use invok::engine::Installer;
use invok::state::{
    start_decision, InstallConfig, InstallState, PreState, StartDecision, StepRecord,
};
use invok::step::Step;
use invok::steps::noop::{NoopStep, UndoLog};

// --- helpers ----------------------------------------------------------------

fn ctx_with_state(dir: &tempfile::TempDir) -> Context {
    Context {
        dry_run: false,
        odoo_version: "18.0".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        ..Default::default()
    }
    .with_state_path(dir.path().join("state.json"))
}

/// the manifest of an interrupted installation: some steps, not finished.
fn partial_state(ctx: &Context, steps: &[(&str, PreState)]) -> InstallState {
    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(ctx));
    for (name, prestate) in steps {
        state.record(StepRecord {
            name: (*name).to_string(),
            snapshot: serde_json::to_value(prestate).expect("serialize"),
        });
    }
    state
}

// --- the policy: install, resume, refuse ------------------------------------

#[test]
fn no_manifest_means_a_first_installation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let decision = start_decision(&InstallState::default(), &cfg, false);

    assert_eq!(decision, StartDecision::Fresh);
}

/// the case A-V3-1 is named for: a second installation over a complete one. it
/// used to be accepted, and the manifest — the only record of what to remove —
/// was rewritten with everything marked pre-existing.
#[test]
fn a_finished_installation_is_refused_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    state.finished = true;

    assert_eq!(
        start_decision(&state, &cfg, false),
        StartDecision::RefuseFinished,
        "reinstalling over a complete instance must stop, not overwrite the manifest"
    );
}

/// `--force` is the explicit way out: reinstall, but set the previous manifest
/// aside. archiving is `main`'s job; this checks the policy demands it.
#[test]
fn force_turns_a_refusal_into_a_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut finita = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    finita.finished = true;
    assert_eq!(start_decision(&finita, &cfg, true), StartDecision::Replace);

    let parziale = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);
    assert_eq!(
        start_decision(&parziale, &cfg, true),
        StartDecision::Replace
    );
}

/// a partial manifest with the same parameters is an interrupted installation,
/// so it resumes. the supported flow, and the one that used to lose ownership
/// of the steps already run.
#[test]
fn a_partial_manifest_with_the_same_parameters_is_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    assert_eq!(start_decision(&state, &cfg, false), StartDecision::Resume);
}

/// resuming with different parameters would produce a manifest straddling two
/// instances, with half the undos pointing elsewhere. for the database that is
/// a direct anti-drop violation.
#[test]
fn resuming_with_different_artifacts_is_refused_and_says_which() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut other = ctx_with_state(&dir);
    other.db_name = "fatturazione".to_string();
    let cfg = InstallConfig::from_context(&other);

    let StartDecision::RefuseIdentityMismatch(differences) = start_decision(&state, &cfg, false)
    else {
        panic!("a refusal on a different identity was expected");
    };

    assert_eq!(differences.len(), 1, "a single difference: the database");
    assert_eq!(
        differences[0],
        (
            "database name",
            "citest".to_string(),
            "fatturazione".to_string()
        ),
        "the message must be able to say WHICH artifact does not match"
    );
}

/// fields that name no artifact do not block a resume: a different port, or a
/// different administrator, does not make the manifest incoherent.
#[test]
fn non_identifying_fields_do_not_block_a_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut diverso = ctx_with_state(&dir);
    diverso.port = 8888;
    diverso.sudo_user = Some("un-other-admin".to_string());
    let cfg = InstallConfig::from_context(&diverso);

    assert_eq!(start_decision(&state, &cfg, false), StartDecision::Resume);
}

/// a pre-R4 manifest with no configuration: identity cannot be established, so
/// fail-closed, as with an unreadable snapshot.
#[test]
fn a_manifest_without_config_is_not_resumed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let cfg = InstallConfig::from_context(&ctx);

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    assert_eq!(
        start_decision(&state, &cfg, false),
        StartDecision::RefuseUnknownIdentity
    );
}

// --- the engine's resume ----------------------------------------------------

/// an already-recorded step is not re-run. observable without spying on the
/// engine: the live step is configured to **fail** its run, so reaching the end
/// proves it was not re-run.
#[test]
fn resume_does_not_re_run_completed_steps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_run()),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::resuming_from(state);
    installer
        .execute(&mut steps, &ctx)
        .expect("alpha has already run: it must not be repeated");

    assert_eq!(
        installer.state().completed.len(),
        2,
        "the manifest must hold alpha (inherited) and beta (run now), once each"
    );
    let names: Vec<&str> = installer
        .state()
        .completed
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(names, vec!["alpha", "beta"]);
}

/// **the defect's test.** on a re-run, a step that already executed would see
/// its own artifact and declare it `Preexisting`: correct as a photograph,
/// disastrous as a verdict of ownership. with the resume, ownership is
/// **re-read** from the manifest and the undo keeps acting.
#[test]
fn resume_inherits_ownership_instead_of_re_deducing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let log: UndoLog = Arc::new(Mutex::new(Vec::new()));

    // the manifest says we created it.
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // the live step is configured as a fresh snapshot would see it today: the
    // artifact is there, hence "pre-existing".
    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(
            NoopStep::new("alpha")
                .preexisting()
                .with_undo_log(Arc::clone(&log)),
        ),
        Box::new(
            NoopStep::new("beta")
                .fail_on_run()
                .with_undo_log(Arc::clone(&log)),
        ),
    ];

    let mut installer = Installer::resuming_from(state);
    let err = installer.execute(&mut steps, &ctx);
    assert!(err.is_err(), "beta fails and triggers the rollback");

    let actions = log.lock().expect("log").clone();
    // `beta` comes first because the failing step is undone too (A-V3-24); the
    // assertion that carries this test is the second entry.
    assert_eq!(
        actions,
        vec!["beta".to_string(), "alpha".to_string()],
        "alpha's undo must ACT: the manifest says the artifact is ours. without the \
         inheritance it would have been Preexisting and the artifact would stay on the \
         machine forever — exactly A-V3-1's damage"
    );
}

/// the recorded configuration is not overwritten by the current one: it is the
/// identity of the artifacts the undos will act on.
#[test]
fn resume_keeps_the_recorded_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let originale = InstallConfig::from_context(&ctx);
    let state = partial_state(&ctx, &[("alpha", PreState::CreatedByUs)]);

    // the engine gets a context naming a different database: overwriting the
    // config would point the undos at the wrong one.
    let mut other = ctx_with_state(&dir);
    other.db_name = "fatturazione".to_string();

    let mut steps: Vec<Box<dyn Step>> = vec![Box::new(NoopStep::new("alpha"))];
    let mut installer = Installer::resuming_from(state);
    installer.execute(&mut steps, &other).expect("execute");

    assert_eq!(
        installer.state().config.as_ref(),
        Some(&originale),
        "the manifest's config is the artifacts' identity: it is not rewritten mid-run"
    );
}

/// an unreadable persisted snapshot stops the resume rather than inventing a
/// verdict: the same fail-closed rule as the rollback from disk.
#[test]
fn an_unreadable_snapshot_stops_the_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let mut state = InstallState::default();
    state.set_config(InstallConfig::from_context(&ctx));
    state.record(StepRecord {
        name: "alpha".to_string(),
        snapshot: serde_json::json!({"non": "un PreState"}),
    });

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha")),
        Box::new(NoopStep::new("beta")),
    ];

    let mut installer = Installer::resuming_from(state);
    assert!(
        installer.execute(&mut steps, &ctx).is_err(),
        "without a readable snapshot we do not know whose the artifact is: better to stop"
    );
}

/// an installation resumed and completed produces a **single** manifest:
/// inherited steps are not duplicated, and the finished flag is set.
#[test]
fn a_resumed_installation_finishes_with_a_single_coherent_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);
    let state = partial_state(
        &ctx,
        &[
            ("alpha", PreState::CreatedByUs),
            ("beta", PreState::Preexisting),
        ],
    );

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("alpha").fail_on_run()),
        Box::new(NoopStep::new("beta").fail_on_run()),
        Box::new(NoopStep::new("gamma")),
    ];

    let mut installer = Installer::resuming_from(state);
    installer.execute(&mut steps, &ctx).expect("execute");
    installer.mark_finished(&ctx).expect("mark_finished");

    let riletto = InstallState::load(&ctx.state_path).expect("load");
    let names: Vec<&str> = riletto.completed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    assert!(riletto.finished, "the resumed installation is finished");

    // the inherited ownership survives the whole pass.
    let alpha: PreState =
        serde_json::from_value(riletto.completed[0].snapshot.clone()).expect("prestate");
    let beta: PreState =
        serde_json::from_value(riletto.completed[1].snapshot.clone()).expect("prestate");
    assert_eq!(alpha, PreState::CreatedByUs);
    assert_eq!(beta, PreState::Preexisting);
}

// --- A-R9-1: a port held by ourselves does not block the resume -------------

/// the port preflight exists to catch a conflict with **somebody else**. once
/// the service step has run, the listener is ours: an installation interrupted
/// after it must not become unresumable because of the service it just
/// installed.
#[test]
fn a_manifest_past_setup_systemd_owns_the_http_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = ctx_with_state(&dir);

    let before = partial_state(
        &ctx,
        &[
            ("prepare-opt-root", PreState::CreatedByUs),
            ("create-database", PreState::CreatedByUs),
        ],
    );
    assert!(
        !before.owns_the_http_port(),
        "without setup-systemd the port is not ours: a conflict is real and must be flagged"
    );

    let dopo = partial_state(
        &ctx,
        &[
            ("prepare-opt-root", PreState::CreatedByUs),
            ("setup-systemd", PreState::CreatedByUs),
        ],
    );
    assert!(
        dopo.owns_the_http_port(),
        "with setup-systemd recorded the port belongs to our own service: the check is \
         skipped, or the resume dies on the service it just installed (A-R9-1)"
    );
}

/// an empty manifest owns nothing: a first installation still goes through the
/// port check.
#[test]
fn an_empty_manifest_owns_no_port() {
    assert!(!InstallState::default().owns_the_http_port());
}

/// the refusal on a completed manifest must offer **all three** ways on.
///
/// the message lives in `main`, where no test can call it, so this reads the
/// source — and the CI job that really re-installs asserts the same three on
/// the real output. structural plus behavioural, as R9 requires: the grep sees
/// the shape, the run sees what comes out.
///
/// `--instance` is the one that was missing, and it is the one most often
/// wanted: whoever runs the installer on a machine that already has Odoo is
/// usually adding a second instance, not undoing or overwriting the first. a
/// refusal that fires correctly and names only the destructive halves is
/// A-R9-1's shape — it sends the reader to fix the wrong thing.
#[test]
fn the_refusal_on_a_finished_manifest_offers_every_way_out() {
    let main = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("src/main.rs must be readable");

    let arm = main
        .split("StartDecision::RefuseFinished")
        .nth(1)
        .expect("the refusal on a completed manifest must exist");
    let arm = arm
        .split("StartDecision::")
        .next()
        .expect("the arm ends at the next decision");
    // the ACTIVE lines, never the comment above them. R14 fell into exactly
    // this: a check on `PermissionsStartOnly` fired on the prose explaining why
    // the directive had been removed. here the comment names `--port` while
    // explaining why the message must, so reading it would keep the guard green
    // with the message stripped — and the mutation that proved it survived is
    // what put this filter here.
    let arm: String = arm
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    for way_out in ["--instance", "rollback", "--force"] {
        assert!(
            arm.contains(way_out),
            "the refusal does not offer '{way_out}': that way out does not exist for whoever \
             reads it"
        );
    }
    // and the port with it: without a free one the next attempt is refused too,
    // by a different check, and a message that earns a second refusal has not
    // done its job.
    assert!(
        arm.contains("--port"),
        "naming --instance without --port sends the user into the port check"
    );
}
