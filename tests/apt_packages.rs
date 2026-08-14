//! the delta pattern: the undo purges only the delta, never the pre-existing
//! packages.

mod common;

use std::collections::HashSet;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::packaging::apt::AptBackend;
use invok::packaging::{PackageManager, PackageSpec};
use invok::state::{InstallState, StepRecord};
use invok::step::Step;
use invok::steps::apt_packages::{AptDeltaSnapshot, AptPackagesStep, UndoPolicy};
use invok::system_ops::{has_installable_candidate, total_package_names};

use common::model::{ModelState, SystemModel};

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
    // one package is already installed, so the delta holds the other two — and
    // the undo may touch only those.
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
    // the delta only.
    assert!(
        ops.contains(&Op::PkgRemove(strings(&["b", "c"]))),
        "atteso purge di [b,c], trovato: {ops:?}"
    );
    // no global autoremove in the rollback: it would take orphans unrelated to
    // Odoo, outside our delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
        "l'undo non deve lanciare un autoremove globale, trovato: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non deve mai essere purgato"
    );
}

// --- A-RT-2: the purge must work with a broken dpkg too ---------------------
//
// found in the field: the rollback arrives here after a later step left dpkg
// inconsistent, apt refuses to operate, the purge fails and the whole delta
// stays installed.

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
        .position(|o| matches!(o, Op::PkgRepair))
        .expect("il recovery di dpkg deve precedere il purge");
    let purge = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRemove(_)))
        .expect("il purge deve essere tentato");
    assert!(
        fix < purge,
        "fix-broken prima del purge, altrimenti apt rifiuta: {ops:?}"
    );
    // and after the recovery the purge really succeeds.
    assert!(
        ops.contains(&Op::PkgRemove(strings(&["b", "c"]))),
        "il delta deve essere purgato dopo il recovery: {ops:?}"
    );
    // the protection holds: never the pre-existing one.
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
        "il pacchetto preesistente 'a' non va mai purgato"
    );
}

#[test]
fn undo_retries_the_purge_after_dpkg_configure_all() {
    // the first repair fails too, so the deeper one runs and the purge is
    // retried.
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
        ops.contains(&Op::PkgDeepRepair),
        "se il fix-broken fallisce si tenta dpkg --configure -a: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::PkgRemove(_))).count(),
        2,
        "il purge va ritentato dopo il recovery: {ops:?}"
    );
}

#[test]
fn undo_stays_best_effort_when_recovery_fails_completely() {
    // no recovery works: the undo **must not fail** (invariant 3), the packages
    // stay and the logs say so.
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
    // all already installed: nothing to install, nothing to purge.
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
    // a normal undo leaves the common bootstrap utilities.
    let cfg = MockConfig::default(); // niente già installato → delta = tutti
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false); // a NON-aggressive rollback

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::PkgInstall(_))),
        "il run deve installare"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::PkgRemove(_))),
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
        ops.iter().any(|op| matches!(op, Op::PkgRemove(_))),
        "con --aggressive-rollback il bootstrap deve purgare il delta"
    );
    // not even aggressively: a global autoremove is outside the delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
        "nessun autoremove globale nemmeno con --aggressive-rollback, trovato: {ops:?}"
    );
}

#[test]
fn deps_delta_excludes_bootstrap_overlap() {
    // a package installed by bootstrap does not enter the dependencies' delta.
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

// --- A5.1: package names that change between OS releases --------------------
//
// confirmed in the field: on a newer release one name lost its candidate and
// another was renamed, so the dependencies step failed on the whole group and
// the installation never started. every branch of the alternatives group is
// exercised here.

/// a step with a single alternatives group, to isolate resolution.
fn step_with_group(mock: MockSystemOps, group: &[&str]) -> AptPackagesStep {
    AptPackagesStep::with_specs(
        Box::new(mock),
        "test-alternatives",
        vec![PackageSpec::any(group)],
        UndoPolicy::PurgeDelta,
    )
}

#[test]
fn the_preferred_name_wins_when_available() {
    // the first alternative exists, so it is used and not the fallback.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(snapshot_of(&step).delta, strings(&["libtiff5-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libtiff5-dev"]))),
        "apt deve ricevere il nome preferito: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_fallback_is_used_when_the_preferred_name_does_not_exist() {
    // no candidate for the first: the fallback is installed and the step does
    // NOT fail — the bug was the installer not starting at all.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["libtiff5-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c)
        .expect("snapshot: il fallback deve salvare lo step");
    assert_eq!(snapshot_of(&step).delta, strings(&["libtiff-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libtiff-dev"]))),
        "apt deve ricevere il fallback, mai il nome inesistente: {:?}",
        ops_of(&log)
    );

    // and the rollback purges the name that was really installed.
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libtiff-dev"]))),
        "il delta persistito è in nomi risolti: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_already_installed_alternative_wins_over_the_preferred_one() {
    // the customer has one alternative. installing the other would be correct
    // but useless, and it would enter the delta — something the rollback then
    // purges from a system that never asked for it.
    let cfg = MockConfig {
        installed_packages: installed(&["libtiff-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libtiff5-dev", "libtiff-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let snap = snapshot_of(&step);
    assert_eq!(snap.already_installed, strings(&["libtiff-dev"]));
    assert!(
        snap.delta.is_empty(),
        "un'alternativa già presente non è nostra: delta vuoto, trovato {:?}",
        snap.delta
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgRemove(_))),
        "niente da purgare: non abbiamo installato nulla. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_group_with_no_installable_alternative_fails_before_mutating() {
    // no name available: the step stops in the *snapshot*, before touching the
    // manager. degrading silently would move the error into a build that does
    // not compile, far harder to diagnose.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["sparito-dev", "sparito2-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::with_specs(
        Box::new(mock),
        "test-alternatives",
        vec![
            PackageSpec::one("presente-dev"),
            PackageSpec::any(&["sparito-dev", "sparito2-dev"]),
        ],
        UndoPolicy::PurgeDelta,
    );

    let err = step
        .snapshot(&ctx(false))
        .expect_err("un gruppo vuoto deve essere un errore");
    let message = err.to_string();
    assert!(
        message.contains("sparito-dev") && message.contains("sparito2-dev"),
        "il messaggio deve dire quale gruppo è vuoto: {message}"
    );
    assert!(
        message.contains("A5.1"),
        "e come chiuderlo (aggiungere l'alternativa): {message}"
    );
    assert!(
        ops_of(&log).is_empty(),
        "nessuna mutazione prima dell'errore: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_unusable_apt_index_is_diagnosed_as_such_not_as_a_missing_package() {
    // A5.1-bis, the heart of the diagnosis. two conditions look identical — "no
    // name has a candidate" — with opposite causes: the package does not exist,
    // or we cannot *see* whether it does. with an unusable index the second is
    // the only lawful conclusion, and the message must say so: the wrong one
    // sent someone hunting a rename that had not happened.
    let cfg = MockConfig {
        apt_index_populated: false, // apt-get update mai eseguito
        ..Default::default()
    };
    let (mock, _log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::custom(
        Box::new(mock),
        "test-indice-inservibile",
        strings(&["libfreetype6-dev", "libxml2-dev"]),
        UndoPolicy::PurgeDelta,
    );

    let message = step
        .snapshot(&ctx(false))
        .expect_err("indice inservibile → errore, ma con la diagnosi giusta")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "con l'indice inservibile il messaggio deve indicare 'apt-get update': {message}"
    );
    assert!(
        !message.contains("non esistono su questa release"),
        "e NON deve dichiarare assenti pacchetti che non ha potuto verificare: {message}"
    );
}

// --- A5.1-bis: the false positive found in CI -------------------------------
//
// two concurrent causes, both real: the runner's package index was never
// refreshed, and on that release one name is purely VIRTUAL — even with a fresh
// index the policy query answers no candidate, while the install command takes
// it without blinking. the fix covers both: a refresh in bootstrap's run, and a
// detection that also asks "could you install it?" before calling it absent.

#[test]
fn bootstrap_updates_the_index_so_the_next_step_sees_the_candidates() {
    // the bug's regression, in its exact shape. on the runner the bootstrap
    // utilities were ALREADY installed, so the delta is empty: with the refresh
    // after the early return the index would stay stale and the dependencies
    // step would reject packages that exist.
    let model = SystemModel::new(ModelState {
        // the runner already carries the bootstrap utilities.
        packages: strings(&["git", "curl", "wget", "gettext-base"])
            .into_iter()
            .collect(),
        apt_index_stale: true, // and nobody ever refreshed the index
        ..Default::default()
    });
    let c = ctx(false);

    let mut bootstrap = AptPackagesStep::bootstrap_with_ops(model.boxed());
    bootstrap
        .snapshot(&c)
        .expect("lo snapshot del bootstrap non deve morire su un indice stantìo");
    assert!(
        snapshot_of(&bootstrap).delta.is_empty(),
        "scenario del bug: le utility bootstrap sono già presenti, delta vuoto"
    );
    bootstrap
        .run(&c)
        .expect("il run del bootstrap aggiorna l'indice anche con delta vuoto");

    // from here the index is fresh and the dependencies step resolves
    // everything.
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(model.boxed());
    deps.snapshot(&c)
        .expect("dopo l'update i candidati ci sono: nessun falso positivo");
    let snap = snapshot_of(&deps);
    assert!(
        snap.delta.contains(&"libfreetype6-dev".to_string()),
        "il pacchetto che in campo veniva bocciato deve essere risolto: {:?}",
        snap.delta
    );
}

#[test]
fn without_the_update_the_dependencies_step_refuses_instead_of_guessing() {
    // the converse: without bootstrap's refresh the dependencies step stops —
    // and stops with the index diagnosis, not by accusing the packages.
    let model = SystemModel::new(ModelState {
        apt_index_stale: true,
        ..Default::default()
    });
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(model.boxed());

    let message = deps
        .snapshot(&ctx(false))
        .expect_err("senza indice lo step dei deps non può decidere")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "diagnosi sull'indice, non sui nomi: {message}"
    );
}

#[test]
fn the_index_update_lives_in_run_never_in_snapshot() {
    // constraint C4: refreshing the index is a mutation, and a snapshot never
    // mutates.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert!(
        !ops_of(&log).contains(&Op::PkgRefreshIndex),
        "lo snapshot deve restare NON mutante: nessun apt-get update. Trovato: {:?}",
        ops_of(&log)
    );

    step.run(&c).expect("run");
    let ops = ops_of(&log);
    let update = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRefreshIndex))
        .expect("il run del bootstrap deve aggiornare l'indice");
    let install = ops
        .iter()
        .position(|o| matches!(o, Op::PkgInstall(_)))
        .expect("e poi installare");
    assert!(
        update < install,
        "l'update va prima dell'install, non dopo: {ops:?}"
    );
}

#[test]
fn only_bootstrap_updates_the_index() {
    // once is enough, from the first step. repeating it per step would be time
    // spent on the network for nothing.
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    let c = ctx(false);

    deps.snapshot(&c).expect("snapshot");
    deps.run(&c).expect("run");
    assert!(
        !ops_of(&log).contains(&Op::PkgRefreshIndex),
        "install-system-dependencies non rifà l'update: {:?}",
        ops_of(&log)
    );
}

#[test]
fn dry_run_does_not_touch_the_apt_index() {
    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = Context {
        dry_run: true,
        ..Default::default()
    };

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    assert!(
        ops_of(&log).is_empty(),
        "dry-run: nessun apt-get update, nessuna install. Trovato: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_third_party_repo_that_fails_does_not_block_the_install() {
    // the refresh exits non-zero for a SINGLE unreachable repository while the
    // official indexes arrived fine. stopping there would hold the installer
    // hostage to somebody's broken third-party source.
    let cfg = MockConfig {
        apt_update_fails: true,
        apt_index_populated: true, // ma l'indice è comunque utilizzabile
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect("un update parziale non deve fermare l'installazione");
    assert!(
        ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "e l'install deve avvenire comunque: {:?}",
        ops_of(&log)
    );
}

#[test]
fn an_update_that_leaves_no_index_at_all_is_a_hard_error() {
    // the other face: with NO index at all, proceeding would leave the later
    // steps deciding blind.
    let cfg = MockConfig {
        apt_update_fails: true,
        apt_index_populated: false,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let message = step
        .run(&c)
        .expect_err("senza indice l'installazione non può procedere")
        .to_string();
    assert!(
        message.contains("apt-get update") && message.contains("indice"),
        "il messaggio deve spiegare cosa manca: {message}"
    );
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "e non si installa nulla dopo l'errore: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_real_name_beats_a_virtual_one_because_only_the_real_one_is_purgeable() {
    // a virtual name is accepted by the install command, but dpkg does not know
    // it and the purge exits zero having removed nothing. a delta carrying that
    // name would lie: the rollback would claim a purge and the real package
    // would stay.
    let cfg = MockConfig {
        virtual_packages: installed(&["libfreetype6-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libfreetype6-dev", "libfreetype-dev"]);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        snapshot_of(&step).delta,
        strings(&["libfreetype-dev"]),
        "vince il nome REALE, non quello virtuale"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libfreetype-dev"]))),
        "e l'undo purga qualcosa che dpkg conosce davvero: {:?}",
        ops_of(&log)
    );
}

#[test]
fn a_virtual_only_group_is_installed_rather_than_refused() {
    // no real name in the group but the manager can install it: refusing would
    // be the field's false positive. proceed with a warning — a blocked
    // installation is certain damage, a leftover only a risk.
    let cfg = MockConfig {
        virtual_packages: installed(&["libfreetype6-dev"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = step_with_group(mock, &["libfreetype6-dev"]);
    let c = ctx(false);

    step.snapshot(&c)
        .expect("un nome virtuale installabile non è un pacchetto assente");
    assert_eq!(snapshot_of(&step).delta, strings(&["libfreetype6-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libfreetype6-dev"]))),
        "apt riceve il nome virtuale, che sa risolvere: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_canonical_list_declares_the_real_name_for_the_virtual_one() {
    // a guard on the list: that name alone would put an unpurgeable entry in
    // the delta, so the real one must be among the alternatives.
    let catalog = AptBackend.catalog();
    let group = catalog
        .odoo_specs()
        .into_iter()
        .find(|s| s.alternatives().iter().any(|n| n == "libfreetype6-dev"))
        .expect("'libfreetype6-dev' deve restare in lista");
    assert!(
        group.alternatives().iter().any(|n| n == "libfreetype-dev"),
        "il nome reale deve essere fra le alternative, trovato {:?}",
        group.alternatives()
    );
}

// --- A-R6-1: the list's refactor must not lose packages ---------------------
//
// R6 rewrote thirty package names by hand, exactly the place where one goes
// missing silently: the mock suite creates no real virtualenv and compiles
// nothing, so the gap would surface in the field with the installation already
// running. these guards sit at the **list** level, where the refactor happens,
// and need no real system to fail.

/// the mandatory set **before** R6, verbatim.
///
/// deliberately frozen: the reference every future rewrite is measured against.
/// if a package disappears the test below says *which*, without waiting for the
/// real CI.
const PRE_R6_REQUIRED: &[&str] = &[
    "git",
    "curl",
    "wget",
    "python3-pip",
    "python3-dev",
    "python3-venv",
    "python3-wheel",
    "python3-setuptools",
    "build-essential",
    "gettext-base",
    "libfreetype6-dev",
    "libxml2-dev",
    "libzip-dev",
    "libldap2-dev",
    "libsasl2-dev",
    "node-less",
    "libjpeg-dev",
    "zlib1g-dev",
    "libpq-dev",
    "libxslt1-dev",
    "libtiff5-dev",
    "libjpeg8-dev",
    "libopenjp2-7-dev",
    "liblcms2-dev",
    "libwebp-dev",
    "libharfbuzz-dev",
    "libfribidi-dev",
    "libxcb1-dev",
    "libev-dev",
    "libc-ares-dev",
];

/// the pre-R6 packages **deliberately** no longer mandatory, with the reason.
/// each entry is a documented decision, not a loss.
const INTENTIONALLY_DEMOTED: &[&str] = &[
    // dropped by some releases; it only compiles .less assets, which modern
    // Odoo does not use. it lives among the optional dependencies.
    "node-less",
];

/// every name appearing in the catalogue's **mandatory** groups.
///
/// it reads the backend's catalogue rather than a constant: since M1 the list
/// is what the manager answers, and a test still looking at a constant would
/// check something nobody consumes.
fn required_names() -> HashSet<String> {
    let catalog = AptBackend.catalog();
    catalog
        .bootstrap_specs()
        .into_iter()
        .chain(catalog.odoo_specs())
        .filter(|spec| spec.is_required())
        .flat_map(|spec| spec.alternatives().to_vec())
        .collect()
}

/// the names in the catalogue's **optional** groups.
fn optional_names() -> Vec<String> {
    AptBackend
        .catalog()
        .odoo_specs()
        .into_iter()
        .filter(|spec| !spec.is_required())
        .flat_map(|spec| spec.alternatives().to_vec())
        .collect()
}

#[test]
fn the_refactor_did_not_lose_a_single_package() {
    // the guard closes the class, not the instance: every package that was
    // mandatory before R6 must still be reachable in a mandatory group, or
    // appear among the explicit demotions.
    let required = required_names();
    let optional: HashSet<String> = optional_names().into_iter().collect();

    let mut lost = Vec::new();
    for pkg in PRE_R6_REQUIRED {
        let demoted = INTENTIONALLY_DEMOTED.contains(pkg);
        if required.contains(*pkg) {
            assert!(
                !demoted,
                "'{pkg}' è marcato come declassato ma è ancora obbligatorio: \
                 aggiorna INTENTIONALLY_DEMOTED o la lista"
            );
            continue;
        }
        if demoted {
            assert!(
                optional.contains(*pkg),
                "'{pkg}' è dichiarato declassato ma non è nemmeno fra gli opzionali"
            );
            continue;
        }
        lost.push(*pkg);
    }

    assert!(
        lost.is_empty(),
        "pacchetti persi rispetto a prima di R6: {lost:?}. \
         Se la rimozione è voluta, dichiarala in INTENTIONALLY_DEMOTED con la ragione"
    );
}

#[test]
fn the_python3_core_set_is_complete() {
    // the Python build set is cohesive: losing one breaks the virtualenv or the
    // wheel builds, and the symptom appears steps later where it is hard to
    // trace back to the package list. one of them is what stands between a
    // working sandbox and one with no interpreter in it (A-R6-1).
    let required = required_names();
    for pkg in [
        "python3-pip",
        "python3-dev",
        "python3-venv",
        "python3-wheel",
        "python3-setuptools",
    ] {
        assert!(
            required.contains(pkg),
            "'{pkg}' deve stare fra le dipendenze OBBLIGATORIE: {:?}",
            required
                .iter()
                .filter(|p| p.starts_with("python3"))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn python3_venv_is_never_optional() {
    // explicit: without the virtualenv there is no Odoo, so demoting it by
    // accident must be impossible.
    let optional = optional_names();
    assert!(
        !optional.iter().any(|p| p.contains("venv")),
        "nessun pacchetto venv può stare fra gli opzionali: {optional:?}"
    );
}

#[test]
fn python3_venv_reaches_apt_whether_or_not_it_is_already_installed() {
    // the list test says "it is in the list"; this says "it reaches the
    // manager", the property that counts — and covers the observation that
    // raised the suspicion: the package was absent from the delta because it
    // was ALREADY installed, not because it was missing.
    for already_present in [false, true] {
        let cfg = MockConfig {
            installed_packages: if already_present {
                installed(&["python3-venv"])
            } else {
                HashSet::new()
            },
            ..Default::default()
        };
        let (mock, log) = MockSystemOps::new(cfg);
        let mut step = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
        let c = ctx(false);

        step.snapshot(&c).expect("snapshot");
        let snap = snapshot_of(&step);
        let seen = snap.delta.contains(&"python3-venv".to_string())
            || snap.already_installed.contains(&"python3-venv".to_string());
        assert!(
            seen,
            "python3-venv deve risultare nello snapshot (delta o già installato), \
             già presente = {already_present}: delta={:?}",
            snap.delta
        );

        step.run(&c).expect("run");
        let installed_line = ops_of(&log)
            .into_iter()
            .find_map(|o| match o {
                Op::PkgInstall(pkgs) => Some(pkgs),
                _ => None,
            })
            .expect("il run deve invocare apt-get install");
        assert!(
            installed_line.contains(&"python3-venv".to_string()),
            "python3-venv deve essere nella riga di apt anche quando è già presente \
             (apt è idempotente), già presente = {already_present}: {installed_line:?}"
        );
    }
}

#[test]
fn apt_cache_stats_output_is_parsed_as_apt_prints_it() {
    // real output of the stats command plus the case that matters: an empty
    // index, which is what separates "I do not know" from "it does not exist".
    let populated =
        "Total package names: 163333 (4573 k)\nTotal package structures: 148622 (6539 k)\n";
    assert_eq!(total_package_names(populated), Some(163333));
    assert_eq!(
        total_package_names("Total package names: 0 (0 B)\n"),
        Some(0)
    );
    assert_eq!(
        total_package_names("E: Impossibile aprire il file di lock\n"),
        None,
        "output senza la riga → non lo sappiamo, che NON è zero"
    );
}

#[test]
fn an_optional_group_without_candidates_does_not_stop_the_install() {
    // the optional one is a nice-to-have: a missing optional is a warning, not
    // an impossible installation, and everything else is still installed.
    let cfg = MockConfig {
        packages_without_candidate: installed(&["node-less"]),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::with_specs(
        Box::new(mock),
        "test-opzionale",
        vec![
            PackageSpec::one("build-essential"),
            PackageSpec::optional(&["node-less"]),
        ],
        UndoPolicy::PurgeDelta,
    );
    let c = ctx(false);

    step.snapshot(&c)
        .expect("un opzionale mancante non è un errore");
    assert_eq!(snapshot_of(&step).delta, strings(&["build-essential"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["build-essential"]))),
        "il pacchetto disponibile va installato comunque: {:?}",
        ops_of(&log)
    );
}

#[test]
fn the_canonical_list_declares_the_renames_seen_in_the_field() {
    // a guard on the production list: the two names that broke a release must
    // keep an alternative, or a future cleanup would reopen A5.1 silently.
    let specs = AptBackend.catalog().odoo_specs();
    for broken in ["libtiff5-dev", "libjpeg8-dev"] {
        let group = specs
            .iter()
            .find(|s| s.alternatives().iter().any(|n| n == broken))
            .unwrap_or_else(|| panic!("'{broken}' deve restare in lista"));
        assert!(
            group.alternatives().len() > 1,
            "'{broken}' non esiste su Debian 12: il suo gruppo deve avere un'alternativa, \
             trovato {:?}",
            group.alternatives()
        );
    }
}

#[test]
fn apt_cache_policy_output_is_parsed_as_apt_prints_it() {
    // the parser runs on real captured output. the three cases that matter are
    // indistinguishable from the exit code alone: available, virtual with no
    // candidate, and non-existent.
    let available =
        "libtiff-dev:\n  Installed: (none)\n  Candidate: 4.5.1+git230720-4ubuntu2.5\n  \
                     Version table:\n     4.5.1+git230720-4ubuntu2.5 500\n";
    let virtual_only = "libjpeg8-dev:\n  Installed: (none)\n  Candidate: (none)\n  \
                        Version table:\n";
    let missing = "N: Unable to locate package libtiff5-dev\n";

    assert!(has_installable_candidate(available));
    assert!(
        !has_installable_candidate(virtual_only),
        "'Candidate: (none)' non è installabile"
    );
    assert!(
        !has_installable_candidate(missing),
        "senza riga Candidate il pacchetto non esiste"
    );
    assert!(!has_installable_candidate(""));
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
