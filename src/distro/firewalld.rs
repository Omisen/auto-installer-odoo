//! the Fedora family's firewall: `firewalld`.
//!
//! the lucky part: the rule token is the same as `ufw`'s, so the
//! `nginx-firewall` step — the delta pattern, hence the protection — does not
//! change a line when the tool underneath does.
//!
//! the costly part: firewalld separates **runtime** from **permanent**
//! configuration. a rule added without `--permanent` disappears on reboot; one
//! added only with it has no effect until a reload. both are needed, which is
//! why this is a trait and not a set of constants.

use super::Firewall;
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// `firewalld`, Fedora's firewall.
#[derive(Debug, Default)]
pub struct Firewalld;

impl Firewall for Firewalld {
    fn name(&self) -> &'static str {
        "firewalld"
    }

    fn available(&self) -> bool {
        std::process::Command::new("firewall-cmd")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// `firewall-cmd --state` exits 0 only while the daemon runs.
    ///
    /// as with `ufw`, installed is not enough: inactive means we do not touch
    /// the firewall at all.
    fn is_active(&self) -> bool {
        std::process::Command::new("firewall-cmd")
            .arg("--state")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// is the rule already among the **permanently** open ports?
    ///
    /// the permanent set, not the runtime one, because it describes the
    /// configuration the customer *wants*: a runtime-only port disappears on
    /// reboot, and treating it as "already there" would stop us opening it
    /// properly.
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let out = capture_command("firewall-cmd", &["--permanent", "--list-ports"])?;
        Ok(port_in_list(&out, rule))
    }

    /// opens the port **permanently**, then reloads to make it effective.
    ///
    /// two commands and not one: `--permanent` writes the configuration without
    /// touching the running firewall, so alone it would leave the port closed
    /// until reboot — nginx unreachable, with nothing to say so. the reload
    /// also keeps [`Self::rule_exists`], which reads the permanent set, in
    /// agreement with what the system is actually doing.
    ///
    /// adding twice, runtime and permanent, would avoid the reload but let the
    /// two states diverge if either command failed.
    fn allow(&self, rule: &str) -> Result<(), StepError> {
        run_command(
            "firewall-cmd",
            &["--permanent", &format!("--add-port={rule}")],
        )?;
        run_command("firewall-cmd", &["--reload"])
    }

    /// closes the port again. called **only** on the delta, never on a rule the
    /// customer already had.
    fn delete(&self, rule: &str) -> Result<(), StepError> {
        run_command(
            "firewall-cmd",
            &["--permanent", &format!("--remove-port={rule}")],
        )?;
        run_command("firewall-cmd", &["--reload"])
    }
}

/// does the port appear in `firewall-cmd --list-ports`?
///
/// compared by **token**, not by substring — the same protection as A-V3-7:
/// `"80/tcp"` is contained in `"8080/tcp"`, and a loose comparison would
/// conclude port 80 was already open where it is not, leaving nginx
/// unreachable **with no error at all**.
///
/// `--list-ports` prints them space-separated on one line:
///
/// ```text
/// 8080/tcp 443/tcp 53/udp
/// ```
///
/// pure, because the interesting case is not reproducible without firewalld.
pub fn port_in_list(list: &str, rule: &str) -> bool {
    list.split_whitespace().any(|token| token == rule)
}
