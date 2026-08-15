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
        "expected a purge of [b,c], found: {ops:?}"
    );
    // no global autoremove in the rollback: it would take orphans unrelated to
    // Odoo, outside our delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
        "the undo must not run a global autoremove, found: {ops:?}"
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
        "the pre-existing package 'a' must never be purged"
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
        dpkg_broken: true, // a later step broke dpkg
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
        .expect("the dpkg recovery must come before the purge");
    let purge = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRemove(_)))
        .expect("the purge must be attempted");
    assert!(
        fix < purge,
        "fix-broken before the purge, or apt refuses: {ops:?}"
    );
    // and after the recovery the purge really succeeds.
    assert!(
        ops.contains(&Op::PkgRemove(strings(&["b", "c"]))),
        "the delta must be purged after the recovery: {ops:?}"
    );
    // the protection holds: never the pre-existing one.
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Op::PkgRemove(p) if p.contains(&"a".to_string()))),
        "the pre-existing package 'a' must never be purged"
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
        "if fix-broken fails, the deeper repair is attempted: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::PkgRemove(_))).count(),
        2,
        "the purge must be retried after the recovery: {ops:?}"
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
        .expect("a best-effort undo never propagates the error");
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
        "an empty delta means no action, found: {:?}",
        ops_of(&log)
    );
}

#[test]
fn bootstrap_does_not_purge_on_normal_undo() {
    // a normal undo leaves the common bootstrap utilities.
    let cfg = MockConfig::default(); // nothing installed, so the delta is all of them
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false); // a NON-aggressive rollback

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::PkgInstall(_))),
        "the run must install"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::PkgRemove(_))),
        "a normal undo must not purge the bootstrap utilities, found: {ops:?}"
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
        "with --aggressive-rollback the bootstrap must purge its delta"
    );
    // not even aggressively: a global autoremove is outside the delta.
    assert!(
        !ops.contains(&Op::PkgRemoveOrphans),
        "no global autoremove even with --aggressive-rollback, found: {ops:?}"
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
        "git must not be in the delta"
    );
    assert!(
        snap.delta.contains(&"python3-pip".to_string()),
        "the other deps stay in the delta"
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
        "apt must receive the preferred name: {:?}",
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
        .expect("snapshot: the fallback must save the step");
    assert_eq!(snapshot_of(&step).delta, strings(&["libtiff-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libtiff-dev"]))),
        "apt must receive the fallback, never the non-existent name: {:?}",
        ops_of(&log)
    );

    // and the rollback purges the name that was really installed.
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libtiff-dev"]))),
        "the persisted delta holds resolved names: {:?}",
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
        "an alternative already present is not ours: an empty delta, found {:?}",
        snap.delta
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgRemove(_))),
        "nothing to purge: we installed nothing. found: {:?}",
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
        .expect_err("an empty group must be an error");
    let message = err.to_string();
    assert!(
        message.contains("sparito-dev") && message.contains("sparito2-dev"),
        "the message must say which group is empty: {message}"
    );
    assert!(
        message.contains("A5.1"),
        "and how to close it (add the alternative): {message}"
    );
    assert!(
        ops_of(&log).is_empty(),
        "no mutation before the error: {:?}",
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
        .expect_err("an unusable index is an error, but with the right diagnosis")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "with an unusable index the message must point at 'apt-get update': {message}"
    );
    assert!(
        !message.contains("do not exist on this release"),
        "and it must NOT call absent packages it could not check: {message}"
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
        .expect("the bootstrap snapshot must not die on a stale index");
    assert!(
        snapshot_of(&bootstrap).delta.is_empty(),
        "the bug's scenario: the bootstrap utilities are already there, an empty delta"
    );
    bootstrap
        .run(&c)
        .expect("the bootstrap run refreshes the index even with an empty delta");

    // from here the index is fresh and the dependencies step resolves
    // everything.
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(model.boxed());
    deps.snapshot(&c)
        .expect("after the refresh the candidates are there: no false positive");
    let snap = snapshot_of(&deps);
    assert!(
        snap.delta.contains(&"libfreetype6-dev".to_string()),
        "the package the field rejected must resolve: {:?}",
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
        .expect_err("without an index the dependencies step cannot decide")
        .to_string();
    assert!(
        message.contains("apt-get update"),
        "a diagnosis about the index, not about the names: {message}"
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
        "the snapshot must stay NON-mutating: no index refresh. found: {:?}",
        ops_of(&log)
    );

    step.run(&c).expect("run");
    let ops = ops_of(&log);
    let update = ops
        .iter()
        .position(|o| matches!(o, Op::PkgRefreshIndex))
        .expect("the bootstrap run must refresh the index");
    let install = ops
        .iter()
        .position(|o| matches!(o, Op::PkgInstall(_)))
        .expect("and then install");
    assert!(
        update < install,
        "the refresh comes before the install, not after: {ops:?}"
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
        "install-system-dependencies does not repeat the refresh: {:?}",
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
        "dry run: no refresh, no install. found: {:?}",
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
        apt_index_populated: true, // but the index is usable anyway
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect("a partial refresh must not stop the installation");
    assert!(
        ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "and the install must happen anyway: {:?}",
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
        .expect_err("without an index the installation cannot proceed")
        .to_string();
    assert!(
        message.contains("apt-get update") && message.contains("index"),
        "the message must explain what is missing: {message}"
    );
    assert!(
        !ops_of(&log).iter().any(|o| matches!(o, Op::PkgInstall(_))),
        "and nothing is installed after the error: {:?}",
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
        "the REAL name wins, not the virtual one"
    );

    step.run(&c).expect("run");
    step.undo(&c).expect("undo");
    assert!(
        ops_of(&log).contains(&Op::PkgRemove(strings(&["libfreetype-dev"]))),
        "and the undo purges something dpkg genuinely knows: {:?}",
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
        .expect("an installable virtual name is not an absent package");
    assert_eq!(snapshot_of(&step).delta, strings(&["libfreetype6-dev"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["libfreetype6-dev"]))),
        "the manager receives the virtual name, which it can resolve: {:?}",
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
        .expect("'libfreetype6-dev' must stay in the list");
    assert!(
        group.alternatives().iter().any(|n| n == "libfreetype-dev"),
        "the real name must be among the alternatives, found {:?}",
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
                "'{pkg}' is marked as demoted but is still mandatory: \
                 update INTENTIONALLY_DEMOTED or the list"
            );
            continue;
        }
        if demoted {
            assert!(
                optional.contains(*pkg),
                "'{pkg}' is declared demoted but is not among the optional ones either"
            );
            continue;
        }
        lost.push(*pkg);
    }

    assert!(
        lost.is_empty(),
        "packages lost since before R6: {lost:?}. \
         if the removal is intended, declare it in INTENTIONALLY_DEMOTED with the reason"
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
            "'{pkg}' must be among the MANDATORY dependencies: {:?}",
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
        "no venv package may sit among the optional ones: {optional:?}"
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
            "python3-venv must appear in the snapshot (delta or already installed), \
             already present = {already_present}: delta={:?}",
            snap.delta
        );

        step.run(&c).expect("run");
        let installed_line = ops_of(&log)
            .into_iter()
            .find_map(|o| match o {
                Op::PkgInstall(pkgs) => Some(pkgs),
                _ => None,
            })
            .expect("the run must invoke the install command");
        assert!(
            installed_line.contains(&"python3-venv".to_string()),
            "python3-venv must be on the install line even when already present \
             (the manager is idempotent), already present = {already_present}: {installed_line:?}"
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
        total_package_names("E: Could not open lock file\n"),
        None,
        "output without the line means we do not know, which is NOT zero"
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
        .expect("a missing optional is not an error");
    assert_eq!(snapshot_of(&step).delta, strings(&["build-essential"]));

    step.run(&c).expect("run");
    assert!(
        ops_of(&log).contains(&Op::PkgInstall(strings(&["build-essential"]))),
        "the available package is installed anyway: {:?}",
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
            .unwrap_or_else(|| panic!("'{broken}' must stay in the list"));
        assert!(
            group.alternatives().len() > 1,
            "'{broken}' does not exist on that release: its group must have an alternative, \
             found {:?}",
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
        "'Candidate: (none)' is not installable"
    );
    assert!(
        !has_installable_candidate(missing),
        "without a Candidate line the package does not exist"
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

// --- the mirror is not the request ------------------------------------------

/// a download that fails on the **mirror's** side is asked again.
///
/// found by the CI, on a `debian:11` probe: `apt-get` got
/// `Connection reset by peer` fetching one `.deb` out of twenty-five, and a
/// whole installation was rolled back for it. Nothing was wrong with the
/// machine, the list, or the code — a mirror closed a socket, which on a
/// customer's line is an ordinary event. The clone has had retries since R2 for
/// exactly this reason; the package manager talks to the network too.
#[test]
fn a_mirror_that_drops_the_connection_is_asked_again() {
    let cfg = MockConfig {
        install_fail_times: 2,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock)).with_retries_for_test(3);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect("two mirror failures must not fail the installation");

    let installs = ops_of(&log)
        .iter()
        .filter(|op| matches!(op, Op::PkgInstall(_)))
        .count();
    assert_eq!(installs, 3, "two failures and the attempt that succeeded");
}

/// and a failure that is **not** the mirror is not retried at all.
///
/// this half matters as much: a name that does not exist answers the same way
/// every time, so asking again only makes the true message arrive three times
/// later, hidden behind a wait. The manager decides which is which, because
/// only it knows the dialect — no step is allowed to match on the family.
#[test]
fn a_package_that_does_not_exist_is_not_retried() {
    let cfg = MockConfig {
        install_fail_times: 5,
        install_failure_stderr: "E: Unable to locate package libfoo-dev".to_string(),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock)).with_retries_for_test(3);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect_err("a package that does not exist must fail");

    let installs = ops_of(&log)
        .iter()
        .filter(|op| matches!(op, Op::PkgInstall(_)))
        .count();
    assert_eq!(installs, 1, "asked once, answered once");
}

/// the retry budget runs out, and the last error is the real one.
#[test]
fn a_mirror_that_never_recovers_reports_its_own_error() {
    let cfg = MockConfig {
        install_fail_times: 99,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = AptPackagesStep::bootstrap_with_ops(Box::new(mock)).with_retries_for_test(3);
    let c = ctx(false);

    step.snapshot(&c).expect("snapshot");
    let err = step.run(&c).expect_err("it must give up in the end");
    assert!(
        err.to_string().contains("Connection reset by peer"),
        "the error handed on must be the mirror's, not one of ours: {err}"
    );
    assert_eq!(
        ops_of(&log)
            .iter()
            .filter(|op| matches!(op, Op::PkgInstall(_)))
            .count(),
        3,
        "three attempts, no more"
    );
}

/// the retry has to be **wired to the real steps**, not merely to exist.
///
/// the mutation that put this here survived every behavioural test: removing
/// `with_retries()` from `build_steps` left three green tests proving a retry
/// that production would never perform. The budget is not observable through
/// `dyn Step`, so this reads the source — paired with the tests above, which
/// prove what the budget does once it is set.
///
/// only the **active** lines: the comment next to those calls names
/// `with_retries` while explaining it, and a check that read the prose would
/// stay green with the calls gone (R14's trap, met twice now).
#[test]
fn the_installing_steps_are_built_with_the_retry_budget() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/steps/mod.rs"),
    )
    .expect("src/steps/mod.rs must be readable");

    let build = source
        .split("pub fn build_steps")
        .nth(1)
        .expect("build_steps must exist");
    let build = build.split("\npub fn ").next().unwrap_or(build);
    let active: String = build
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let with_retries = active.matches(".with_retries()").count();
    assert_eq!(
        with_retries, 2,
        "both package steps must be built with the retry budget: a mirror that drops one \
         download out of twenty-five otherwise costs a whole installation"
    );
}
