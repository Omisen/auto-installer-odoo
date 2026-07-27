//! Test del pattern delta (Fase 4): l'undo purga solo il delta, mai i preesistenti.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use odoo_installer::context::Context;
use odoo_installer::state::{InstallState, StepRecord};
use odoo_installer::step::Step;
use odoo_installer::steps::apt_packages::{AptDeltaSnapshot, AptPackagesStep, UndoPolicy};

fn ctx(aggressive: bool) -> Context {
    Context {
        dry_run: false,
        aggressive_rollback: aggressive,
        ..Default::default()
    }
}

fn installed(pkgs: &[&str]) -> HashSet<String> {
    pkgs.iter().map(|s| s.to_string()).collect()
}

fn snapshot_of(step: &AptPackagesStep) -> AptDeltaSnapshot {
    serde_json::from_value(step.snapshot_value()).expect("snapshot serializzabile")
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn undo_purges_only_the_delta() {
    // 'a' già installato → delta = [b, c]. L'undo deve purgare solo [b, c].
    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.already_installed, strings(&["a"]));
    assert_eq!(snap.delta, strings(&["b", "c"]));

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    // Purge del solo delta, mai 'a'.
    assert!(
        ops.contains(&Op::AptPurge(strings(&["b", "c"]))),
        "atteso purge di [b,c], trovato: {ops:?}"
    );
    // A3.3/A3.2: nessun `apt-get autoremove` globale nel rollback — rimuoverebbe
    // orfani estranei a Odoo, fuori dal nostro delta.
    assert!(
        !ops.contains(&Op::AptAutoremove),
        "l'undo non deve lanciare un autoremove globale, trovato: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::AptPurge(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non deve mai essere purgato"
    );
}

// --- A-RT-2: il purge deve funzionare anche con dpkg rotto ------------------
//
// Trovato in campo (Multipass, Ubuntu 22.04): il rollback arriva qui dopo che
// uno step a valle ha lasciato dpkg in stato inconsistente, apt si rifiuta di
// operare, il purge fallisce e i 24 pacchetti del delta restano installati.

#[test]
fn undo_recovers_a_broken_dpkg_before_purging() {
    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        dpkg_broken: true, // uno step a valle ha rotto dpkg
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo best-effort");

    let ops = ops_of(&log);
    let fix = ops
        .iter()
        .position(|o| matches!(o, Op::AptFixBroken))
        .expect("il recovery di dpkg deve precedere il purge");
    let purge = ops
        .iter()
        .position(|o| matches!(o, Op::AptPurge(_)))
        .expect("il purge deve essere tentato");
    assert!(
        fix < purge,
        "fix-broken prima del purge, altrimenti apt rifiuta: {ops:?}"
    );
    // E il purge, dopo il recovery, riesce davvero: il delta se ne va.
    assert!(
        ops.contains(&Op::AptPurge(strings(&["b", "c"]))),
        "il delta deve essere purgato dopo il recovery: {ops:?}"
    );
    // La protezione resta: mai il preesistente.
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::AptPurge(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non va mai purgato"
    );
}

#[test]
fn undo_retries_the_purge_after_dpkg_configure_all() {
    // `apt-get install -f` non basta (fallisce a sua volta): si passa a
    // `dpkg --configure -a` e si ritenta il purge.
    let cfg = MockConfig {
        dpkg_broken: true,
        fix_broken_fails: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo best-effort");

    let ops = ops_of(&log);
    assert!(
        ops.contains(&Op::DpkgConfigureAll),
        "se il fix-broken fallisce si tenta dpkg --configure -a: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::AptPurge(_))).count(),
        2,
        "il purge va ritentato dopo il recovery: {ops:?}"
    );
}

#[test]
fn undo_stays_best_effort_when_recovery_fails_completely() {
    // Nessun recovery funziona: l'undo **non deve fallire** (invariante 3), i
    // pacchetti restano e l'utente lo scopre dai log.
    let cfg = MockConfig {
        dpkg_broken: true,
        fix_broken_fails: true,
        dpkg_configure_fails: true,
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-delta",
        strings(&["b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c)
        .expect("undo best-effort: non propaga mai l'errore");
}

#[test]
fn empty_delta_is_noop() {
    // Tutti già installati → niente install, niente purge.
    let cfg = MockConfig {
        installed_packages: installed(&["a", "b", "c"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-empty",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert!(snapshot_of(&step).delta.is_empty());
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    assert!(
        ops_of(&log).is_empty(),
        "delta vuoto → nessuna azione, trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn bootstrap_does_not_purge_on_normal_undo() {
    // Bootstrap: undo normale non purga git/curl/wget/gettext.
    let cfg = MockConfig::default(); // niente già installato → delta = tutti
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false); // rollback NON aggressivo

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AptInstall(_))),
        "il run deve installare"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::AptPurge(_))),
        "undo normale non deve purgare le utility bootstrap, trovato: {ops:?}"
    );
}

#[test]
fn bootstrap_purges_only_with_aggressive_rollback() {
    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(true); // --aggressive-rollback

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AptPurge(_))),
        "con --aggressive-rollback il bootstrap deve purgare il delta"
    );
    // Nemmeno in modalità aggressiva: l'autoremove globale è fuori dal delta.
    assert!(
        !ops.contains(&Op::AptAutoremove),
        "nessun autoremove globale nemmeno con --aggressive-rollback, trovato: {ops:?}"
    );
}

#[test]
fn deps_delta_excludes_bootstrap_overlap() {
    // 'git' già installato (dal bootstrap) → non finisce nel delta dei deps.
    let cfg = MockConfig {
        installed_packages: installed(&["git"]),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);

    assert!(snap.already_installed.contains(&"git".to_string()));
    assert!(
        !snap.delta.contains(&"git".to_string()),
        "git non deve essere nel delta"
    );
    assert!(
        snap.delta.contains(&"python3-pip".to_string()),
        "gli altri deps restano nel delta"
    );
}

#[test]
fn delta_survives_state_save_load() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let cfg = MockConfig {
        installed_packages: installed(&["a"]),
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "persisted",
        strings(&["a", "b", "c"]),
        UndoPolicy::PurgeDelta,
    );
    step.snapshot(&Context::default()).expect("snapshot");

    let mut state = InstallState::default();
    state.record(StepRecord {
        name: step.name().to_string(),
        snapshot: step.snapshot_value(),
    });
    state.save(&path).expect("save");

    let reloaded = InstallState::load(&path).expect("load");
    let snap: AptDeltaSnapshot =
        serde_json::from_value(reloaded.completed[0].snapshot.clone()).expect("delta");
    assert_eq!(snap.delta, strings(&["b", "c"]));
    assert_eq!(snap.already_installed, strings(&["a"]));
}
