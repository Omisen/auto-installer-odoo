//! [`NginxFirewall`] (13d): apre le porte su ufw, col **pattern delta**.
//!
//! # Mutazione insidiosa: regole ufw
//!
//! Le regole firewall sono config di sistema del cliente. Come per l'apt di
//! Fase 4, si applica il pattern delta: l'undo rimuove **solo** le regole che
//! abbiamo aggiunto noi (il delta), **mai** una regola che il cliente aveva già.
//!
//! Se ufw non è installato o non è attivo → no-op (non forziamo il firewall).

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::step::Step;
use crate::system_ops::{RealSystemOps, SystemOps};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct NginxFirewallSnapshot {
    /// Regole che aggiungiamo noi (non presenti prima): le sole rimovibili.
    pub delta: Vec<String>,
}

pub struct NginxFirewall {
    ops: Box<dyn SystemOps>,
    snap: NginxFirewallSnapshot,
}

impl NginxFirewall {
    pub fn new() -> Self {
        Self::with_ops(Box::new(RealSystemOps::new()))
    }
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            snap: NginxFirewallSnapshot::default(),
        }
    }

    fn desired_rules(ctx: &Context) -> Vec<&'static str> {
        let mut rules = vec!["80/tcp"];
        if ctx.nginx_enable_ssl {
            rules.push("443/tcp");
        }
        rules
    }

    /// ufw è utilizzabile (installato e attivo)?
    fn ufw_usable(&self) -> bool {
        if !self.ops.ufw_available() {
            warn!("ufw non trovato: apertura firewall saltata (apri manualmente 80/443)");
            return false;
        }
        if !self.ops.ufw_is_active() {
            warn!("ufw presente ma non attivo: apertura firewall saltata");
            return false;
        }
        true
    }
}

impl Default for NginxFirewall {
    fn default() -> Self {
        Self::new()
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
        // Delta = regole desiderate che NON sono già presenti.
        for rule in Self::desired_rules(ctx) {
            if !self.ops.ufw_rule_exists(rule)? {
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
            self.ops.ufw_allow(rule)?;
        }
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!("undo (dry-run): rimuoverei solo le regole del delta");
            return Ok(());
        }
        // Rimuovi SOLO il delta: mai una regola preesistente del cliente.
        for rule in &self.snap.delta {
            if let Err(e) = self.ops.ufw_delete(rule) {
                warn!(rule = %rule, error = %e, "undo: rimozione regola ufw fallita, proseguo (best-effort)");
            }
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }
}
