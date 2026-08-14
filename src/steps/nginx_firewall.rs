//! [`NginxFirewall`]: opens the ports through the **delta pattern**.
//!
//! firewall rules are the customer's system configuration, so the same pattern
//! as the packages applies: the undo removes **only** the rules we added,
//! **never** one that was already there.
//!
//! an absent or inactive firewall means a no-op: we do not force one on.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxFirewallSnapshot {
    /// the rules we add, and the only ones removable.
    pub delta: Vec<String>,
}

pub struct NginxFirewall {
    ops: Box<dyn SystemOps>,
    snap: NginxFirewallSnapshot,
}

impl NginxFirewall {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxFirewallSnapshot::default(),
        }
    }

    fn desired_rules(ctx: &Context) -> Vec<&'static str> {
        let mut rules = vec!["80/tcp"];
        if ctx.nginx_open_https_port {
            rules.push("443/tcp");
        }
        rules
    }

    /// is the firewall usable, i.e. installed and active?
    fn ufw_usable(&self) -> bool {
        let distro = self.ops.distro();
        let firewall = distro.firewall();
        // the name comes from the tool, not a constant: naming the wrong one
        // sends the reader looking for something their machine does not have.
        let nome = firewall.name();
        if !firewall.available() {
            warn!("{nome} non trovato: apertura firewall saltata (apri manualmente 80/443)");
            return false;
        }
        if !firewall.is_active() {
            warn!("{nome} presente ma non attivo: apertura firewall saltata");
            return false;
        }
        true
    }
}

impl Step for NginxFirewall {
    fn name(&self) -> &str {
        "nginx-firewall"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        self.snap = NginxFirewallSnapshot::default();
        if !ctx.with_nginx || !self.ufw_usable() {
            return Ok(());
        }
        // the delta is the wanted rules that are NOT already there.
        for rule in Self::desired_rules(ctx) {
            if !self.ops.distro().firewall().rule_exists(rule)? {
                self.snap.delta.push(rule.to_string());
            }
        }
        info!(delta = ?self.snap.delta, "snapshot nginx-firewall");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-firewall");
            return Ok(());
        }
        if ctx.dry_run {
            info!(delta = ?self.snap.delta, "run (dry-run): aprirei le regole del delta");
            return Ok(());
        }
        for rule in &self.snap.delta {
            self.ops.distro().firewall().allow(rule)?;
        }
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rimuoverei solo le regole del delta");
            return Ok(());
        }
        // remove ONLY the delta: never a pre-existing rule of the customer's.
        for rule in &self.snap.delta {
            if let Err(e) = self.ops.distro().firewall().delete(rule) {
                warn!(rule = %rule, error = %e, "undo: rimozione regola ufw fallita, proseguo (best-effort)");
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// the same reasoning as the package delta: it is **re-read**, not
    /// recomputed. after the run every wanted rule exists, so a recomputed
    /// delta would have the undo close a port the customer opened themselves.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
