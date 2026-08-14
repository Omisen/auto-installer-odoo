//! [`NginxSelinux`]: lets nginx reach Odoo where SELinux forbids it.
//!
//! # the defect it closes, found in the field
//!
//! SELinux is enforcing and denies nginx a connection to a local service on an
//! unreserved port:
//!
//! ```text
//! avc: denied { name_connect } for comm="nginx" dest=8069
//!      scontext=httpd_t tcontext=unreserved_port_t permissive=0
//! ```
//!
//! the vhost is correct, `nginx -t` passes, the reload succeeds — and the
//! browser gets **502**. nothing looks wrong in the installer's logs: a defect
//! with no symptom until the first user, the class this project fears most
//! (A-V3-7).
//!
//! a step and not one more command, because `setsebool -P` writes the policy
//! **persistently** and survives a reboot. it is a mutation of the customer's
//! system like any other and needs its own `PreState`, or it would be something
//! we switch on and nobody switches off.
//!
//! the `Preexisting` case is not theoretical: on a machine already hosting a
//! reverse proxy that boolean is almost certainly on, and turning it off during
//! a rollback would break somebody else's proxy.

use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

/// turns on the SELinux boolean that permits the proxy, reversibly.
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
            info!("snapshot: SELinux is not in use on this family, the step is a no-op");
            return Ok(());
        };

        let boolean = selinux.nginx_proxy_boolean();
        match selinux.is_enabled(boolean) {
            // already on means not ours, and turning it off would break
            // somebody else's proxy.
            Some(true) => {
                self.prestate = PreState::Preexisting;
                info!(
                    boolean,
                    "snapshot: the SELinux boolean is already on, it is not ours"
                );
            }
            Some(false) => {
                self.prestate = PreState::Untracked;
                info!(
                    boolean,
                    "snapshot: the SELinux boolean is off, we will turn it on"
                );
            }
            // unqueryable is not "off": without an answer we do not touch the
            // security policy of a system we cannot read.
            None => {
                self.prestate = PreState::Untracked;
                warn!(
                    boolean,
                    "snapshot: SELinux cannot be queried (getsebool absent, or the policy is \
                     disabled): nothing is touched. if the proxy answers 502, this is the \
                     first place to look"
                );
            }
        }
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if !ctx.with_nginx {
            info!("nginx not requested, skipping nginx-selinux");
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

        // with a policy the snapshot could not read we write nothing:
        // `Untracked` covers both "off" and "unknown".
        if selinux.is_enabled(boolean).is_none() {
            return Ok(());
        }

        if ctx.dry_run {
            info!(boolean, "run (dry run): would turn the SELinux boolean on");
            return Ok(());
        }

        selinux.set(boolean, true)?;
        self.prestate = PreState::CreatedByUs;
        info!(
            boolean,
            "run: SELinux boolean turned on (nginx can reach Odoo)"
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        // PROTECTION: turn off ONLY what we turned on. one that was already on
        // serves somebody else.
        if self.prestate != PreState::CreatedByUs {
            info!(
                prestate = ?self.prestate,
                "undo NO-OP: the SELinux boolean was not turned on by us"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry run): would turn the SELinux boolean off");
            return Ok(());
        }

        let distro = self.ops.distro();
        let Some(selinux) = distro.selinux() else {
            return Ok(());
        };
        let boolean = selinux.nginx_proxy_boolean();

        if let Err(e) = selinux.set(boolean, false) {
            warn!(boolean, error = %e, "undo: turning the SELinux boolean off failed, proceeding (best-effort)");
        } else {
            info!(boolean, "undo: the SELinux boolean was turned back off");
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
