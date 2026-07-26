//! [`AptPackagesStep`]: installa un insieme di pacchetti apt, in modo
//! reversibile, applicando il **pattern delta**.
//!
//! È la generalizzazione del `PreState` singolo a un *insieme*: lo snapshot non
//! è più "esiste sì/no" ma "quale sottoinsieme era già installato prima di me".
//! L'undo rimuove **solo il delta** — i pacchetti che NON c'erano prima e che
//! quindi abbiamo aggiunto noi — mai i preesistenti.
//!
//! Due configurazioni (una lista in un solo posto, come `_apt_packages_odoo`):
//! - [`AptPackagesStep::bootstrap`] — utility comuni (git/curl/wget/gettext).
//!   Undo **non purga** di default (decisione D3): non si disinstalla git/curl
//!   da una macchina cliente per un rollback. Purga solo con
//!   `--aggressive-rollback`.
//! - [`AptPackagesStep::odoo_dependencies`] — i ~30 pacchetti dev di Odoo. È il
//!   **delta pesante**: l'undo purga il delta + `autoremove`.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

/// Prerequisiti bootstrap: utility comuni a basso rischio.
pub const BOOTSTRAP_PACKAGES: &[&str] = &["git", "curl", "wget", "gettext-base"];

/// Dipendenze di sistema di Odoo (lista canonica, da `system.sh`).
pub const ODOO_DEPENDENCIES: &[&str] = &[
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

/// Politica di undo per l'insieme di pacchetti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoPolicy {
    /// Purga sempre il delta + autoremove (delta pesante di Odoo).
    PurgeDelta,
    /// Non purga di default; purga il delta solo con `--aggressive-rollback`
    /// (utility bootstrap comuni).
    KeepUnlessAggressive,
}

/// Snapshot serializzabile del pattern delta (invariante 4).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AptDeltaSnapshot {
    /// Pacchetti già presenti prima di noi: **da non toccare mai** nell'undo.
    pub already_installed: Vec<String>,
    /// Pacchetti che NON c'erano: ciò che installiamo e possiamo rimuovere.
    pub delta: Vec<String>,
}

/// Step di installazione pacchetti apt con pattern delta.
pub struct AptPackagesStep {
    ops: Box<dyn SystemOps>,
    name: &'static str,
    packages: Vec<String>,
    policy: UndoPolicy,
    snap: AptDeltaSnapshot,
}

impl AptPackagesStep {
    /// Prerequisiti bootstrap (undo non purga senza `--aggressive-rollback`).
    pub fn bootstrap() -> Self {
        Self::bootstrap_with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn bootstrap_with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self::custom(
            ops,
            "bootstrap-prerequisites",
            BOOTSTRAP_PACKAGES.iter().map(|s| s.to_string()).collect(),
            UndoPolicy::KeepUnlessAggressive,
        )
    }

    /// Dipendenze di sistema di Odoo (undo purga il delta + autoremove).
    pub fn odoo_dependencies() -> Self {
        Self::odoo_dependencies_with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn odoo_dependencies_with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self::custom(
            ops,
            "install-system-dependencies",
            ODOO_DEPENDENCIES.iter().map(|s| s.to_string()).collect(),
            UndoPolicy::PurgeDelta,
        )
    }

    /// Costruttore generico (usato anche dai test con liste ad hoc).
    pub fn custom(
        ops: Box<dyn SystemOps>,
        name: &'static str,
        packages: Vec<String>,
        policy: UndoPolicy,
    ) -> Self {
        Self {
            ops,
            name,
            packages,
            policy,
            snap: AptDeltaSnapshot::default(),
        }
    }

    /// Purga il delta persistito + autoremove (best-effort). Usa il delta dello
    /// snapshot, **non** ricalcolato dallo stato corrente (che nel frattempo è
    /// cambiato: il run ha installato i pacchetti).
    fn purge_delta(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.delta.is_empty() {
            info!(step = self.name, "undo: delta vuoto, niente da purgare");
            return Ok(());
        }
        if ctx.dry_run {
            info!(step = self.name, delta = ?self.snap.delta, "undo (dry-run): purgerei il delta + autoremove");
            return Ok(());
        }
        let refs: Vec<&str> = self.snap.delta.iter().map(String::as_str).collect();
        if let Err(e) = self.ops.apt_purge(&refs) {
            warn!(step = self.name, error = %e, "undo: apt purge fallito, proseguo (best-effort)");
        }
        if let Err(e) = self.ops.apt_autoremove() {
            warn!(step = self.name, error = %e, "undo: apt autoremove fallito, proseguo (best-effort)");
        }
        Ok(())
    }
}

impl Step for AptPackagesStep {
    fn name(&self) -> &str {
        self.name
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        let mut already_installed = Vec::new();
        let mut delta = Vec::new();
        for pkg in &self.packages {
            if self.ops.dpkg_is_installed(pkg) {
                already_installed.push(pkg.clone());
            } else {
                delta.push(pkg.clone());
            }
        }
        info!(
            step = self.name,
            already = already_installed.len(),
            delta = delta.len(),
            "snapshot delta apt"
        );
        self.snap = AptDeltaSnapshot {
            already_installed,
            delta,
        };
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
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
                "run (dry-run): apt-get install dell'intera lista (installa solo i mancanti)"
            );
            return Ok(());
        }
        // Installa l'intera lista: apt aggiunge solo i mancanti (idempotente).
        let refs: Vec<&str> = self.packages.iter().map(String::as_str).collect();
        self.ops.apt_install(&refs)?;
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
}
