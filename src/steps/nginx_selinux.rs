//! [`NginxSelinux`]: lascia che nginx raggiunga Odoo, dove SELinux lo vieta.
//!
//! # Il difetto che chiude (trovato in campo su Fedora 41)
//!
//! Su Fedora SELinux è in enforcing e nega a nginx di aprire una connessione
//! verso un servizio locale su una porta non riservata:
//!
//! ```text
//! avc: denied { name_connect } for comm="nginx" dest=8069
//!      scontext=httpd_t tcontext=unreserved_port_t permissive=0
//! ```
//!
//! Il vhost è corretto, `nginx -t` passa, `nginx-reload` riesce — e il browser
//! riceve **502 Bad Gateway**. Nei log dell'installer non c'è niente di anomalo
//! da leggere: è un difetto senza sintomo fino al primo utente, cioè la classe
//! che questo progetto ha imparato a temere di più (A-V3-7).
//!
//! # Perché è uno step e non un comando in più
//!
//! `setsebool -P` scrive la politica **in modo persistente**: sopravvive al
//! riavvio. È quindi una mutazione del sistema del cliente come le altre, e
//! richiede un `PreState` proprio — altrimenti sarebbe qualcosa che accendiamo
//! noi e che nessuno spegne (A-R5-3, applicato a una politica di sicurezza).
//!
//! Il caso `Preexisting` non è teorico: su una macchina che ospita già un
//! reverse proxy quel boolean è quasi certamente acceso, e spegnerlo al rollback
//! romperebbe il proxy di qualcun altro.
//!
//! # Perché non esisteva prima
//!
//! Perché fino alla prima installazione reale con nginx su Fedora **nessuno
//! l'aveva osservato**. Scriverlo prima sarebbe stato mitigare un problema
//! ipotetico, e questo progetto ha una regola contro i rami che nessuno ha visto
//! eseguire.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// Accende il boolean SELinux che permette il proxy, in modo reversibile.
pub struct NginxSelinux {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl NginxSelinux {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }
}

impl Step for NginxSelinux {
    fn name(&self) -> &str {
        "nginx-selinux"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        self.prestate = PreState::Untracked;
        if !ctx.with_nginx {
            return Ok(());
        }

        let distro = self.ops.distro();
        let Some(selinux) = distro.selinux() else {
            info!("snapshot: su questa famiglia SELinux non è in uso, step no-op");
            return Ok(());
        };

        let boolean = selinux.nginx_proxy_boolean();
        match selinux.is_enabled(boolean) {
            // Già acceso: non è nostro. Su una macchina che ospita altri servizi
            // web lo è quasi sempre, e spegnerlo al rollback romperebbe il proxy
            // di qualcun altro.
            Some(true) => {
                self.prestate = PreState::Preexisting;
                info!(
                    boolean,
                    "snapshot: boolean SELinux già attivo, non è nostro"
                );
            }
            Some(false) => {
                self.prestate = PreState::Untracked;
                info!(
                    boolean,
                    "snapshot: boolean SELinux spento, lo accenderemo noi"
                );
            }
            // Non interrogabile ≠ spento. Senza una risposta non si tocca la
            // politica di sicurezza di un sistema che non sappiamo leggere: è la
            // stessa distinzione fra cecità e assenza di A5.1-bis.
            None => {
                self.prestate = PreState::Untracked;
                warn!(
                    boolean,
                    "snapshot: SELinux non interrogabile (getsebool assente o politica \
                     disabilitata): non tocco nulla. Se il proxy risponde 502, è il primo \
                     posto dove guardare"
                );
            }
        }
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx non richiesto, skip nginx-selinux");
            return Ok(());
        }
        if self.prestate != PreState::Untracked {
            return Ok(());
        }

        let distro = self.ops.distro();
        let Some(selinux) = distro.selinux() else {
            return Ok(());
        };
        let boolean = selinux.nginx_proxy_boolean();

        // Se lo snapshot non ha potuto leggere la politica, non la scriviamo:
        // `Untracked` qui vale sia «spento» sia «non lo so», e nel dubbio non si
        // muta il sistema di qualcun altro.
        if selinux.is_enabled(boolean).is_none() {
            return Ok(());
        }

        if ctx.dry_run {
            info!(boolean, "run (dry-run): accenderei il boolean SELinux");
            return Ok(());
        }

        selinux.set(boolean, true)?;
        self.prestate = PreState::CreatedByUs;
        info!(
            boolean,
            "run: boolean SELinux acceso (nginx può raggiungere Odoo)"
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // PROTEZIONE: si spegne SOLO ciò che abbiamo acceso noi. Un boolean già
        // attivo prima di noi serve a qualcun altro.
        if self.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.prestate,
                "undo NO-OP: boolean SELinux non acceso da noi"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry-run): spegnerei il boolean SELinux");
            return Ok(());
        }

        let distro = self.ops.distro();
        let Some(selinux) = distro.selinux() else {
            return Ok(());
        };
        let boolean = selinux.nginx_proxy_boolean();

        if let Err(e) = selinux.set(boolean, false) {
            warn!(boolean, error = %e, "undo: spegnimento del boolean SELinux fallito, proseguo (best-effort)");
        } else {
            info!(boolean, "undo: boolean SELinux rimesso a spento");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        self.prestate = decode_snapshot(self.name(), snapshot)?;
        Ok(())
    }
}
