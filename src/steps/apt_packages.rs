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
        "I nomi elencati non esistono su questa release: aggiungi il nome corretto come \
         alternativa nel catalogo della famiglia (A5.1)"
            .to_string()
    } else {
        format!(
            "L'indice dei pacchetti non è interrogabile, quindi NON è detto che i pacchetti \
             manchino davvero: esegui '{refresh_command}' e riprova. Se l'aggiornamento non \
             produce un indice valido, il problema è la rete o i repository configurati"
        )
    };
    StepError::Precondition(format!(
        "nessun pacchetto installabile per {} {}. {cause}",
        if unavailable.len() == 1 {
            "il gruppo"
        } else {
            "i gruppi"
        },
        groups.join(", "),
    ))
}

/// removes duplicates **preserving order**; the first occurrence wins.
///
/// `Vec::dedup` would not do: it only removes **consecutive** duplicates, and
/// here the two colliding names are six positions apart.
pub fn dedup_keeping_order(names: &mut Vec<String>) {
    let mut visti = std::collections::HashSet::new();
    names.retain(|name| visti.insert(name.clone()));
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
        }
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
        let mut virtuale: Option<&String> = None;
        for name in spec.alternatives() {
            match pm.availability(name) {
                Availability::Real => return Some(ResolvedPackage::Installable(name.clone())),
                Availability::VirtualOnly if virtuale.is_none() => virtuale = Some(name),
                _ => {}
            }
        }
        virtuale.map(|name| ResolvedPackage::Virtual(name.clone()))
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
                "run (dry-run): aggiornerei l'indice dei pacchetti"
            );
            return Ok(());
        }
        let Err(e) = self.ops.packages().refresh_index() else {
            info!(step = self.name, "run: indice dei pacchetti aggiornato");
            return Ok(());
        };
        if self.ops.packages().index_is_queryable() {
            warn!(
                step = self.name,
                error = %e,
                "run: l'aggiornamento dell'indice ha segnalato errori (repository \
                 irraggiungibile?), ma l'indice resta interrogabile: proseguo"
            );
            return Ok(());
        }
        Err(StepError::Precondition(format!(
            "l'aggiornamento dell'indice ({}) è fallito e l'indice resta vuoto: senza indice \
             non è possibile stabilire quali pacchetti siano installabili. Verifica rete e \
             repository configurati. Errore originale: {e}",
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
            info!(step = self.name, "undo: delta vuoto, niente da purgare");
            return Ok(());
        }
        if ctx.dry_run {
            info!(step = self.name, delta = ?self.snap.delta, "undo (dry-run): purgerei il delta");
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
                        pacchetto = %name,
                        gruppo = ?spec.alternatives(),
                        "snapshot: nessun candidato reale nel gruppo, uso un nome VIRTUALE. \
                         È installabile, ma il purge dell'undo potrebbe non reclamarlo: \
                         considera di aggiungere il nome reale come alternativa"
                    );
                    resolved.push(name.clone());
                    delta.push(name);
                }
                None if spec.is_required() => unavailable.push(spec.clone()),
                None => warn!(
                    step = self.name,
                    gruppo = ?spec.alternatives(),
                    "snapshot: nessuna alternativa disponibile per un gruppo OPZIONALE, proseguo senza"
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
                        gruppo = ?spec.alternatives(),
                        "snapshot: indice dei pacchetti non interrogabile, non posso verificare \
                         questo gruppo. Uso il nome preferito e lascio decidere al gestore nel run \
                         (dopo l'aggiornamento dell'indice)"
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
                        gruppo = ?spec.alternatives(),
                        "snapshot: indice dei pacchetti non interrogabile e gruppo OPZIONALE, \
                         proseguo senza"
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
            risolti = resolved.len(),
            "snapshot delta pacchetti"
        );
        // the NAMES, not just the count: the log is the run's **journal**, the
        // manifest is its **state**. confusing the two cost A-R8-1. after a
        // rollback the manifest correctly lists nothing, but "which packages
        // did we add" stays a legitimate question.
        info!(
            step = self.name,
            pacchetti = %delta.join(" "),
            "delta pacchetti: pacchetti aggiunti da noi"
        );
        info!(
            step = self.name,
            pacchetti = %already_installed.join(" "),
            "delta pacchetti: pacchetti già presenti, mai toccati"
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
                "run: tutti i pacchetti già presenti, niente da installare"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                step = self.name,
                "run (dry-run): installerei l'intera lista (il gestore aggiunge solo i mancanti)"
            );
            return Ok(());
        }
        // install the whole resolved list: only the missing ones are added.
        let refs: Vec<&str> = self.resolved.iter().map(String::as_str).collect();
        self.ops.packages().install(&refs)?;
        info!(
            step = self.name,
            installed = self.snap.delta.len(),
            "run: pacchetti installati"
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
                        "undo NO-OP: le utility comuni (git/curl/wget/gettext) restano installate. \
                         Usa --aggressive-rollback per purgare anche queste."
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
