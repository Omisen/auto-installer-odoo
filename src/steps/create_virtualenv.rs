//! [`CreateVirtualenv`]: creates the Python virtualenv in
//! `<install_dir>/sandbox`.
//!
//! everything runs as the **odoo** user. its undo also covers
//! [`InstallPythonRequirements`](crate::steps::install_python_requirements):
//! removing the venv removes every package inside it.

use tracing::{info, warn};

use crate::context::Context;
use crate::distro::OsFamily;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::SystemOps;

const VENV_SUBDIR: &str = "sandbox";

/// creates the virtualenv, reversibly.
pub struct CreateVirtualenv {
    ops: Box<dyn SystemOps>,
    prestate: PreState,
}

impl CreateVirtualenv {
    pub fn with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self {
            ops,
            prestate: PreState::Untracked,
        }
    }

    fn venv_dir(ctx: &Context) -> std::path::PathBuf {
        ctx.install_dir.join(VENV_SUBDIR)
    }
}

/// where `ensurepip` is expected to come from, on this family.
///
/// the precondition's **question** is the same everywhere — can this system
/// create a virtualenv, answered by `import ensurepip` (A-R6-1) — but the
/// suggestion is not: on one family a package is missing, on the other
/// `ensurepip` is in the stdlib and an absence means something else. the wrong
/// advice sends people looking for a package that does not exist.
///
/// pure, and checkable for both families without having either at hand.
pub fn missing_ensurepip_hint(family: OsFamily) -> &'static str {
    match family {
        OsFamily::Debian => {
            "on Debian/Ubuntu it comes with the python3-venv package (or its versioned \
             variant, such as python3.12-venv)"
        }
        OsFamily::Fedora => {
            "on Fedora there is no python3-venv package: ensurepip lives in python3-libs, \
             which should already be there. if it is missing, the Python installation is \
             incomplete or a non-system python3 was used"
        }
    }
}

impl Step for CreateVirtualenv {
    fn name(&self) -> &str {
        "create-virtualenv"
    }

    fn snapshot(&mut self, ctx: &Context) -> Result<(), StepError> {
        let venv = Self::venv_dir(ctx);
        self.prestate = if self.ops.venv_python_exists(&venv) {
            PreState::Preexisting
        } else {
            PreState::Untracked
        };
        info!(venv = %venv.display(), prestate = ?self.prestate, "snapshot create-virtualenv");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!("run: virtualenv already present, skipping");
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                python = %ctx.python.command,
                "run (dry run): <python> -m venv <install_dir>/sandbox"
            );
            return Ok(());
        }

        // the question is "can this system create a virtualenv?", answered by
        // `import ensurepip` — not by the presence of the `venv` module, which
        // is in the stdlib and always there. while this asked about the wrong
        // module it could not fail, and the failure arrived later as a raw
        // Python error with a half-built `sandbox` (A-R6-1).
        //
        // the interpreter is the planned one (M11), not a hardcoded `python3`:
        // asking `ensurepip` of an interpreter other than the one we will use
        // is the right answer to the wrong question.
        let python = &ctx.python.command;
        if !self.ops.python_venv_available(python) {
            return Err(StepError::Precondition(format!(
                "cannot create a virtualenv with `{python}`: the 'ensurepip' module is missing. \
                 {}. the install-system-dependencies step should have made it available: check \
                 its outcome in the log",
                missing_ensurepip_hint(ctx.os_family)
            )));
        }

        let venv = Self::venv_dir(ctx);
        self.ops.create_venv(&ctx.odoo_user, python, &venv)?;
        self.prestate = PreState::CreatedByUs;
        info!(venv = %venv.display(), python = %python, "run: virtualenv created");
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (venv not created by us)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry run): rm -rf of the virtualenv");
            return Ok(());
        }
        // removing the venv removes every pip package inside it.
        let venv = Self::venv_dir(ctx);
        if let Err(e) = self.ops.remove_dir_all(&venv) {
            warn!(error = %e, "undo: rm -rf of the virtualenv failed, proceeding (best-effort)");
        }
        Ok(())
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.prestate).unwrap_or(serde_json::Value::Null)
    }

    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let prestate = decode_snapshot(self.name(), snapshot)?;
        self.prestate = prestate;
        Ok(())
    }
}
