//! [`AptPackagesStep`]: installs a set of packages reversibly, applying the
//! **delta pattern**.
//!
//! the single `PreState` generalised to a *set*: the snapshot no longer asks
//! "does it exist" but "which subset was already installed before me". the undo
//! removes **only the delta** — what was not there and so was added by us —
//! never the pre-existing packages.
//!
//! two configurations, each taking its list from the **package manager's
//! catalogue** rather than a constant here: package names are the manager's
//! knowledge as much as its commands are.
//!
//! - bootstrap prerequisites: common utilities, whose undo **does not purge**
//!   by default (decision D3). you do not uninstall git from a customer's
//!   machine over a rollback.
//! - Odoo's system dependencies: the **heavy delta**, whose undo purges the
//!   delta and **only** the delta.
//!
//! # portable package names (A5.1)
//!
//! the same package is not named the same on every release, and while the list
//! was bare strings `apt-get install` failed on the **whole group** at the
//! first unknown name — confirmed in the field, where the installer would not
//! start on Debian.
//!
//! so the list is made of [`PackageSpec`]s: groups of **alternatives in order
//! of preference**. the snapshot resolves each group to one concrete name, and
//! everything downstream — install, delta, purge, persistence — works on
//! resolved names and never knows alternatives existed.
//!
//! resolution has three rules, in order (the reasoning is on `resolve`): an
//! already-installed alternative wins; otherwise the first with **real**
//! availability; otherwise the first the manager can install anyway, i.e. a
//! **virtual** name, as a fallback because a virtual name cannot be removed
//! (A5.1-bis).
//!
//! with no alternative available the step **fails in the snapshot**, before
//! mutating, naming the empty group. degrading silently would turn a missing
//! `-dev` into a compilation error inside `pip install`, far harder to trace.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::packaging::{Availability, PackageSpec};
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// how a group of alternatives resolved on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedPackage {
    /// one alternative is already installed: it stays out of the delta.
    AlreadyInstalled(String),
    /// none installed, but this one has a **real** candidate: it enters the
    /// delta.
    Installable(String),
    /// none installed and no real candidate, but the manager can install this
    /// name because it is **virtual**. it enters the delta with a caveat — see
    /// [`AptPackagesStep::resolve`].
    Virtual(String),
}

/// builds the error for groups with no installable alternative.
///
/// the message depends on **how much we know**, not on how many groups fell:
/// with an unqueryable index there is no evidence of absence, and claiming the
/// package does not exist would be an invented diagnosis (A5.1-bis).
fn unavailable_packages_error(
    unavailable: &[PackageSpec],
    index_populated: bool,
    refresh_command: &str,
) -> StepError {
    let groups: Vec<String> = unavailable
        .iter()
        .map(|spec| format!("[{}]", spec.alternatives().join(" | ")))
        .collect();
    let cause = if index_populated {
        "the listed names do not exist on this release: add the right one as an \
         alternative in the family's catalogue (A5.1)"
            .to_string()
    } else {
        format!(
            "the package index cannot be queried, so those packages are NOT necessarily \
             missing: run '{refresh_command}' and try again. if the refresh produces no \
             valid index, the problem is the network or the configured repositories"
        )
    };
    StepError::Precondition(format!(
        "no installable package for {} {}. {cause}",
        if unavailable.len() == 1 {
            "group"
        } else {
            "groups"
        },
        groups.join(", "),
    ))
}

/// removes duplicates **preserving order**; the first occurrence wins.
///
/// `Vec::dedup` would not do: it only removes **consecutive** duplicates, and
/// here the two colliding names are six positions apart.
pub fn dedup_keeping_order(names: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
}

/// the undo policy for this set of packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoPolicy {
    /// always purges the delta, and only the delta.
    PurgeDelta,
    /// does not purge by default; purges the delta only under
    /// `--aggressive-rollback`.
    KeepUnlessAggressive,
}

/// the delta pattern's serialisable snapshot (invariant 4).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AptDeltaSnapshot {
    /// packages present before us: **never** touched by the undo.
    pub already_installed: Vec<String>,
    /// packages that were NOT there: what we install and may remove.
    pub delta: Vec<String>,
}

/// a package installation step following the delta pattern.
pub struct AptPackagesStep {
    ops: Box<dyn SystemOps>,
    name: &'static str,
    specs: Vec<PackageSpec>,
    policy: UndoPolicy,
    snap: AptDeltaSnapshot,
    /// names resolved by the snapshot, in the specs' order: what `run` hands to
    /// the manager. in memory only — a rollback from disk never calls `run`,
    /// and the purge reads the persisted delta.
    resolved: Vec<String>,
    /// whether `run` refreshes the index first. set only on the first package
    /// step: from there the fresh index serves every step downstream.
    refresh_index: bool,
    /// whether this step's list adapts to the chosen interpreter (M11): the
    /// system Python's headers out, the alternative and its own in.
    adapts_python: bool,
    /// how many times the install is attempted when the **mirror** is what
    /// failed, and how long to wait between attempts.
    ///
    /// zero backoff in the test constructor, exactly as `CloneOdooRepo` does:
    /// three instant attempts against an unresponsive network are one attempt,
    /// so the real value has to come from the real constructor — and tests must
    /// not sleep.
    install_attempts: u32,
    backoff_base_secs: u64,
}

/// how many times a package install is attempted before giving up.
const DEFAULT_INSTALL_ATTEMPTS: u32 = 3;
/// seconds waited after the first failed attempt; it grows linearly.
const DEFAULT_BACKOFF_SECS: u64 = 5;

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

impl AptPackagesStep {
    /// bootstrap prerequisites; the undo does not purge without
    /// `--aggressive-rollback`.
    ///
    /// the list comes from the manager's catalogue, not from a constant here.
    pub fn bootstrap_with_ops(ops: Box<dyn SystemOps>) -> Self {
        let bootstrap = ops.packages().catalog().bootstrap_specs();
        let mut step = Self::with_specs(
            ops,
            "bootstrap-prerequisites",
            bootstrap,
            UndoPolicy::KeepUnlessAggressive,
        );
        // the first package step: the index is refreshed here, for everyone.
        step.refresh_index = true;
        step
    }

    /// Odoo's system dependencies; the undo purges the delta, and only it.
    pub fn odoo_dependencies_with_ops(ops: Box<dyn SystemOps>) -> Self {
        let odoo = ops.packages().catalog().odoo_specs();
        let mut step = Self::with_specs(
            ops,
            "install-system-dependencies",
            odoo,
            UndoPolicy::PurgeDelta,
        );
        // THIS step carries the alternative interpreter, not the bootstrap one:
        // its undo purges the delta, while the bootstrap's leaves what it
        // added. an interpreter installed and never removed would be a 43 MB
        // leftover inside the perimeter the rollback promises to clean.
        step.adapts_python = true;
        step
    }

    /// generic constructor over bare names, one per group, for tests with ad
    /// hoc lists where alternatives do not matter.
    pub fn custom(
        ops: Box<dyn SystemOps>,
        name: &'static str,
        packages: Vec<String>,
        policy: UndoPolicy,
    ) -> Self {
        let specs = packages.iter().map(|p| PackageSpec::one(p)).collect();
        Self::with_specs(ops, name, specs, policy)
    }

    /// generic constructor over groups of alternatives.
    pub fn with_specs(
        ops: Box<dyn SystemOps>,
        name: &'static str,
        specs: Vec<PackageSpec>,
        policy: UndoPolicy,
    ) -> Self {
        Self {
            ops,
            name,
            specs,
            policy,
            snap: AptDeltaSnapshot::default(),
            resolved: Vec::new(),
            refresh_index: false,
            adapts_python: false,
            // the test constructor: no waiting, and a single attempt, so a test
            // that does not care about retries sees exactly one call.
            install_attempts: 1,
            backoff_base_secs: 0,
        }
    }

    /// the retry budget, with **no** waiting: for the tests, which must not
    /// sleep, and which are about the decision rather than the pause.
    pub fn with_retries_for_test(mut self, attempts: u32) -> Self {
        self.install_attempts = attempts.max(1);
        self.backoff_base_secs = 0;
        self
    }

    /// the constructor `build_steps` uses: it reads the retry budget from the
    /// environment and applies a real backoff between attempts.
    pub fn with_retries(mut self) -> Self {
        self.install_attempts =
            env_u32("PACKAGE_INSTALL_ATTEMPTS", DEFAULT_INSTALL_ATTEMPTS).max(1);
        self.backoff_base_secs = DEFAULT_BACKOFF_SECS;
        self
    }

    /// installs the resolved list, asking again when the **mirror** was what
    /// failed.
    ///
    /// found by the CI, on a `debian:11` probe: `apt-get` got
    /// `Connection reset by peer` fetching one `.deb` out of twenty-five, and a
    /// whole installation was rolled back for it. Nothing was wrong with the
    /// machine, the list or the code — a mirror closed a socket. On a customer's
    /// line that is not rare, and it is exactly the shape `CloneOdooRepo`
    /// already handles for the clone: the network is the part that fails.
    ///
    /// what is **not** retried is as important: a name that does not exist, a
    /// dependency that cannot be satisfied, a broken `dpkg`. Those answer the
    /// same way every time, and asking again only makes the true message arrive
    /// three times later. The manager decides which is which
    /// ([`crate::packaging::PackageManager::is_transient_failure`]) because only
    /// it knows the dialect.
    fn install_resolved(&self, refs: &[&str]) -> Result<(), StepError> {
        for attempt in 1..=self.install_attempts {
            let error = match self.ops.packages().install(refs) {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            let stderr = match &error {
                StepError::CommandFailed { stderr, .. } => stderr.clone(),
                _ => String::new(),
            };
            let transient = self.ops.packages().is_transient_failure(&stderr);
            if !transient || attempt == self.install_attempts {
                return Err(error);
            }
            warn!(
                step = self.name,
                attempt,
                attempts = self.install_attempts,
                error = %error,
                "run: the package download failed on the mirror's side, asking again"
            );
            if self.backoff_base_secs > 0 {
                std::thread::sleep(std::time::Duration::from_secs(
                    attempt as u64 * self.backoff_base_secs,
                ));
            }
        }
        // unreachable: the loop returns on the last attempt.
        Ok(())
    }

    /// picks, inside a group, the name to use on **this** system.
    ///
    /// three questions, in this order:
    ///
    /// 1. **"do you already have one?"** if so it wins, so a customer with
    ///    `libtiff-dev` does not also get `libtiff5-dev` padding the delta.
    /// 2. **"which has a real candidate?"** the fast path, covering the normal
    ///    cases.
    /// 3. **"which could you install anyway?"** the slow path, covering
    ///    **virtual** names.
    ///
    /// a real name beats a virtual one (A5.1-bis) because a virtual name is
    /// installable but **not purgeable**, which breaks the delta pattern in
    /// silence: the purge exits 0 having removed nothing, so the rollback would
    /// report success while the real package stayed installed — an invisible
    /// leftover, the worst kind.
    ///
    /// so level 3 is a **fallback**, taken only when no alternative in the
    /// group has a real candidate, and it is logged.
    fn resolve(&self, spec: &PackageSpec) -> Option<ResolvedPackage> {
        let pm = self.ops.packages();
        if let Some(installed) = spec
            .alternatives()
            .iter()
            .find(|name| pm.is_installed(name))
        {
            return Some(ResolvedPackage::AlreadyInstalled(installed.clone()));
        }
        // one pass: take the **first** real name and keep the first virtual one
        // aside. two passes would reach the same verdict at twice the
        // questions.
        let mut virtual_name: Option<&String> = None;
        for name in spec.alternatives() {
            match pm.availability(name) {
                Availability::Real => return Some(ResolvedPackage::Installable(name.clone())),
                Availability::VirtualOnly if virtual_name.is_none() => virtual_name = Some(name),
                _ => {}
            }
        }
        virtual_name.map(|name| ResolvedPackage::Virtual(name.clone()))
    }

    /// refreshes the package index before installing (A5.1-bis).
    ///
    /// in the **first** package step's `run`, so that when the next one queries
    /// candidates the index is already fresh. putting it in a snapshot would be
    /// more convenient and **wrong**: a snapshot never mutates (C4).
    ///
    /// tolerant of unreachable repositories: a refresh exits non-zero over a
    /// single broken third-party repository while the official indices
    /// downloaded fine. so a failure with a populated index is a `warn!`, and
    /// only **no** index at all is an error.
    fn refresh_apt_index(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!(
                step = self.name,
                "run (dry run): would refresh the package index"
            );
            return Ok(());
        }
        let Err(e) = self.ops.packages().refresh_index() else {
            info!(step = self.name, "run: package index refreshed");
            return Ok(());
        };
        if self.ops.packages().index_is_queryable() {
            warn!(
                step = self.name,
                error = %e,
                "run: the index refresh reported errors (unreachable repository?), but the \
                 index is still queryable: proceeding"
            );
            return Ok(());
        }
        Err(StepError::Precondition(format!(
            "the index refresh ({}) failed and the index is still empty: without an index \
             there is no way to tell which packages are installable. check the network and \
             the configured repositories. original error: {e}",
            self.ops.packages().refresh_command()
        )))
    }

    /// purges the persisted delta, best-effort.
    ///
    /// uses the snapshot's delta and **not** one recomputed from the current
    /// state, which `run` has meanwhile changed.
    ///
    /// no global `autoremove` (A3.2): it acts on the whole system, removing
    /// anything apt considers orphaned *at that moment*, including packages
    /// pulled in by other software. that removal would not be bounded by our
    /// delta — the exact opposite of the surgical principle.
    ///
    /// removal goes through
    /// [`remove_with_recovery`](crate::steps::remove_with_recovery), which
    /// repairs a manager a downstream step may have broken (A-RT-2).
    fn purge_delta(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.delta.is_empty() {
            info!(step = self.name, "undo: empty delta, nothing to purge");
            return Ok(());
        }
        if ctx.dry_run {
            info!(step = self.name, delta = ?self.snap.delta, "undo (dry run): would purge the delta");
            return Ok(());
        }
        let refs: Vec<&str> = self.snap.delta.iter().map(String::as_str).collect();
        crate::steps::remove_with_recovery(self.ops.packages(), self.name, &refs);
        Ok(())
    }
}

impl Step for AptPackagesStep {
    fn name(&self) -> &str {
        self.name
    }

    /// resolves the groups **and** computes the delta: two operations that must
    /// happen at the same instant, before any mutation. the delta is expressed
    /// in resolved names, so the undo's purge never needs to know alternatives
    /// existed.
    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let mut already_installed = Vec::new();
        let mut delta = Vec::new();
        let mut resolved = Vec::new();
        let mut unavailable = Vec::new();

        // the final list is composed here and not in the constructor: the
        // Python plan is decided at preflight, while the steps are built
        // earlier. with the system interpreter the list comes back unchanged.
        let specs = if self.adapts_python {
            ctx.python.adapt_specs(&self.specs)
        } else {
            self.specs.clone()
        };

        for spec in &specs {
            match self.resolve(spec) {
                Some(ResolvedPackage::AlreadyInstalled(name)) => {
                    resolved.push(name.clone());
                    already_installed.push(name);
                }
                Some(ResolvedPackage::Installable(name)) => {
                    resolved.push(name.clone());
                    delta.push(name);
                }
                Some(ResolvedPackage::Virtual(name)) => {
                    warn!(
                        step = self.name,
                        package = %name,
                        group = ?spec.alternatives(),
                        "snapshot: no real candidate in the group, using a VIRTUAL name. it is \
                         installable, but the undo's purge may not reclaim it: consider adding \
                         the real name as an alternative"
                    );
                    resolved.push(name.clone());
                    delta.push(name);
                }
                None if spec.is_required() => unavailable.push(spec.clone()),
                None => warn!(
                    step = self.name,
                    group = ?spec.alternatives(),
                    "snapshot: no alternative available for an OPTIONAL group, proceeding without it"
                ),
            }
        }

        // before declaring a group absent: do we know enough to say so? with an
        // unqueryable index the answer is no, and the verdict would be
        // blindness dressed as diagnosis (A5.1-bis).
        //
        // which step this is decides what to do with that blindness. the
        // bootstrap one **fixes the index itself**, in its own `run`, and its
        // snapshot necessarily runs first: stopping here would make installing
        // on a fresh machine impossible. every other step runs after the
        // refresh, so an unusable index there is a real problem.
        if !unavailable.is_empty() {
            let index_populated = self.ops.packages().index_is_queryable();
            if index_populated || !self.refresh_index {
                return Err(unavailable_packages_error(
                    &unavailable,
                    index_populated,
                    self.ops.packages().refresh_command(),
                ));
            }
            for spec in unavailable.drain(..) {
                if spec.is_required() {
                    warn!(
                        step = self.name,
                        group = ?spec.alternatives(),
                        "snapshot: the package index cannot be queried, so this group cannot be \
                         checked. using the preferred name and letting the manager decide in the \
                         run, after the index refresh"
                    );
                    let preferred = spec.preferred().to_string();
                    resolved.push(preferred.clone());
                    delta.push(preferred);
                } else {
                    // an unverifiable optional is skipped: adding it would fail
                    // the WHOLE install if it turned out not to exist, which is
                    // the opposite of optional.
                    warn!(
                        step = self.name,
                        group = ?spec.alternatives(),
                        "snapshot: the package index cannot be queried and the group is OPTIONAL, \
                         proceeding without it"
                    );
                }
            }
        }

        // deduplicate (A-MD-1): two groups can resolve to the **same name**,
        // and the "do you have one already?" rule cannot help, because the
        // snapshot resolves *every* group before `run` installs anything.
        //
        // the duplicate is harmless to the manager, but the delta is the
        // accounting of what we added and what the undo may act on — and
        // accounting with a doubled line is wrong accounting.
        dedup_keeping_order(&mut resolved);
        dedup_keeping_order(&mut already_installed);
        dedup_keeping_order(&mut delta);

        info!(
            step = self.name,
            already = already_installed.len(),
            delta = delta.len(),
            resolved_count = resolved.len(),
            "snapshot: package delta"
        );
        // the NAMES, not just the count: the log is the run's **journal**, the
        // manifest is its **state**. confusing the two cost A-R8-1. after a
        // rollback the manifest correctly lists nothing, but "which packages
        // did we add" stays a legitimate question.
        info!(
            step = self.name,
            packages = %delta.join(" "),
            "package delta: packages added by us"
        );
        info!(
            step = self.name,
            packages = %already_installed.join(" "),
            "package delta: packages already there, never touched"
        );
        self.resolved = resolved;
        self.snap = AptDeltaSnapshot {
            already_installed,
            delta,
        };
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        // BEFORE any early return: the index serves the steps downstream, not
        // us. on a runner the bootstrap utilities are already installed, so an
        // early return would leave the next step querying a stale index and
        // rejecting packages that exist (A5.1-bis).
        if self.refresh_index {
            self.refresh_apt_index(ctx)?;
        }

        if self.snap.delta.is_empty() {
            info!(
                step = self.name,
                "run: every package already present, nothing to install"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                step = self.name,
                "run (dry run): would install the whole list (the manager adds only what is missing)"
            );
            return Ok(());
        }
        // install the whole resolved list: only the missing ones are added.
        let refs: Vec<&str> = self.resolved.iter().map(String::as_str).collect();
        self.install_resolved(&refs)?;
        info!(
            step = self.name,
            installed = self.snap.delta.len(),
            "run: packages installed"
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        match self.policy {
            UndoPolicy::PurgeDelta => self.purge_delta(ctx),
            UndoPolicy::KeepUnlessAggressive => {
                if ctx.aggressive_rollback {
                    self.purge_delta(ctx)
                } else {
                    info!(
                        step = self.name,
                        "undo NO-OP: the common utilities (git/curl/wget/gettext) stay installed. \
                         use --aggressive-rollback to purge those too."
                    );
                    Ok(())
                }
            }
        }
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// rehydrates the **delta**: the packages that were not there before us.
    ///
    /// recomputing it would be wrong by construction — after `run` the whole
    /// list is installed, so the delta would look empty, or worse would match
    /// the whole list and have the undo purge the customer's packages.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
