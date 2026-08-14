//! the Debian family's firewall: `ufw`.

use super::Firewall;
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// `ufw`, Debian and Ubuntu's firewall.
#[derive(Debug, Default)]
pub struct Ufw;

impl Firewall for Ufw {
    fn name(&self) -> &'static str {
        "ufw"
    }

    fn available(&self) -> bool {
        std::process::Command::new("ufw")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn is_active(&self) -> bool {
        capture_command("ufw", &["status"])
            .map(|s| s.contains("Status: active"))
            .unwrap_or(false)
    }

    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let status = capture_command("ufw", &["status"])?;
        Ok(rule_in_status(&status, rule))
    }

    fn allow(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["allow", rule])
    }

    fn delete(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["delete", "allow", rule])
    }
}

/// does the rule appear in `ufw status`? compared by **token**, not by
/// substring (A-V3-7).
///
/// `status.contains("80/tcp")` answers `true` on a machine that only has an
/// `8080/tcp` rule. from there the port 80 rule never enters the delta, `run`
/// never opens it, and nginx is configured and reloaded correctly while
/// staying **unreachable from outside** — with nothing odd in the report.
///
/// # how `ufw status` reads
///
/// ```text
/// Status: active
///
/// To                         Action      From
/// --                         ------      ----
/// 80/tcp                     ALLOW       Anywhere
/// 8080/tcp                   ALLOW       Anywhere
/// 80/tcp (v6)                ALLOW       Anywhere (v6)
/// ```
///
/// the rule is the line's **first token**, the `To` column. the `(v6)` suffix
/// is a token of its own, so the IPv6 variant of a port matches — correctly,
/// since it is the same rule.
///
/// declared limitation: an application profile such as `Nginx Full` has a
/// space in the `To` column and will not be recognised as `80/tcp`, even
/// though it opens that port. we would then add `80/tcp` to the delta and the
/// undo would remove it — still only what we added, so the surgical promise
/// holds.
pub fn rule_in_status(status: &str, rule: &str) -> bool {
    /// tokens that appear in the first column without being rules.
    const INTESTAZIONI: [&str; 3] = ["To", "--", "Status:"];

    status
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|token| !INTESTAZIONI.contains(token))
        .any(|token| token == rule)
}
