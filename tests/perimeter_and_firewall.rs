//! A-V3-7 and A-V3-8: two checks that looked like they were doing their job.
//!
//! different findings of the same shape — a comparison that is **too loose**.
//! the first asked "does this string appear somewhere?" instead of "is this
//! rule present?"; the second compared two values that both came **from the
//! same file**, so they always agreed.

use std::path::PathBuf;

use invok::distro::ufw::rule_in_status as ufw_rule_in_status;
use invok::state::{trust_verdict, InstallConfig};

// --- A-V3-7: `80/tcp` is not inside `8080/tcp` ------------------------------

/// realistic `ufw status` output.
fn ufw_status(rules: &[&str]) -> String {
    let mut out = String::from("Status: active\n\nTo                         Action      From\n--                         ------      ----\n");
    for r in rules {
        out.push_str(&format!("{r:<26} ALLOW       Anywhere\n"));
    }
    out
}

/// **the defect.** a substring check answers `true` on a machine that only has
/// `8080/tcp` — another web app, a reverse proxy, a runner.
///
/// the consequence is not cosmetic: the rule never enters the delta, the run
/// never opens it, and nginx is configured and reloaded correctly while staying
/// **unreachable from outside**. nothing looks wrong in the report, which is
/// the worst part.
#[test]
fn port_80_is_not_found_inside_port_8080() {
    let status = ufw_status(&["8080/tcp", "22/tcp"]);

    assert!(
        !ufw_rule_in_status(&status, "80/tcp"),
        "80/tcp is NOT present: it is only a substring of 8080/tcp"
    );
    assert!(ufw_rule_in_status(&status, "8080/tcp"));
    assert!(ufw_rule_in_status(&status, "22/tcp"));
}

/// the IPv6 variant of the same port matches: it is the same rule, so reopening
/// it would duplicate and removing it would touch something we did not add.
#[test]
fn the_ipv6_variant_is_the_same_rule() {
    let status = ufw_status(&["80/tcp", "80/tcp (v6)"]);
    assert!(ufw_rule_in_status(&status, "80/tcp"));
}

/// an absent rule stays absent, even on empty or inactive output.
#[test]
fn an_absent_rule_is_reported_absent() {
    assert!(!ufw_rule_in_status(&ufw_status(&[]), "443/tcp"));
    assert!(!ufw_rule_in_status("", "443/tcp"));
    assert!(!ufw_rule_in_status("Status: inactive\n", "443/tcp"));
}

/// the status heading must never match a rule.
#[test]
fn the_header_is_not_mistaken_for_a_rule() {
    let status = ufw_status(&["80/tcp"]);
    for header in ["To", "--", "Status:"] {
        assert!(
            !ufw_rule_in_status(&status, header),
            "'{header}' is a heading, not a rule"
        );
    }
}

// --- A-V3-8: anchoring the perimeter to something not from the file ---------

fn config_with(home: &str, install_dir: &str) -> InstallConfig {
    InstallConfig {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        odoo_home: PathBuf::from(home),
        install_dir: PathBuf::from(install_dir),
        port: 8069,
        odoo_logfile: None,
        with_nginx: false,
        sudo_user: None,
        os_family: invok::distro::OsFamily::Debian,
        installer_version: None,
    }
}

/// **the defect.** the perimeter net demands the target sit under the home —
/// but both come **from the same state file**, so the guard always agreed with
/// itself: with a home of `/`, a target of `/etc` passed without objection.
///
/// the only possible anchor is a value that does **not** come from the file.
#[test]
fn a_manifest_declaring_another_home_is_refused() {
    let bugiardo = config_with("/", "/etc");
    let err = bugiardo
        .validate_perimeter()
        .expect_err("a manifest declaring '/' as the home is not ours");

    let msg = err.to_string();
    assert!(
        msg.contains("/opt/odoo"),
        "the message must say what the real perimeter is: {msg}"
    );
}

/// a *plausible* but different home is refused too: the constant is
/// architectural and not overridable, so any other value describes an
/// installation we did not make.
#[test]
fn even_a_plausible_but_different_home_is_refused() {
    assert!(config_with("/srv/odoo", "/srv/odoo/odoo18")
        .validate_perimeter()
        .is_err());
}

/// the install directory must sit **below** the home and not equal it, or its
/// undo would take the whole home away.
#[test]
fn the_install_dir_must_live_strictly_inside_the_home() {
    assert!(config_with("/opt/odoo", "/opt/altro")
        .validate_perimeter()
        .is_err());
    assert!(config_with("/opt/odoo", "/opt/odoo")
        .validate_perimeter()
        .is_err());
    assert!(config_with("/opt/odoo", "/opt/odoo/odoo18")
        .validate_perimeter()
        .is_ok());
}

// --- A-V3-8: the state file as a trusted source -----------------------------

/// the good case: root-owned, `0600`, in a directory third parties cannot
/// write.
///
/// checkable only because the rule takes the permissions as parameters: a file
/// created by a test belongs to whoever runs it, never to root.
#[test]
fn a_root_owned_private_file_is_trusted() {
    assert!(trust_verdict(0, 0o100600, Some(0o40755)).is_ok());
    assert!(trust_verdict(0, 0o100640, Some(0o40750)).is_ok());
}

/// another user's file does not drive destructive operations.
#[test]
fn a_file_owned_by_someone_else_is_refused() {
    let err = trust_verdict(1000, 0o100600, Some(0o40755)).expect_err("uid non-root");
    assert!(err.contains("root"), "{err}");
}

/// group- or world-writable: whoever can rewrite it chooses what we delete.
#[test]
fn a_world_or_group_writable_file_is_refused() {
    assert!(trust_verdict(0, 0o100666, Some(0o40755)).is_err());
    assert!(trust_verdict(0, 0o100620, Some(0o40755)).is_err());
}

/// the directory matters as much as the file: in a world-writable one it can be
/// **replaced** without being writable.
#[test]
fn a_file_in_a_world_writable_directory_is_refused() {
    let err = trust_verdict(0, 0o100600, Some(0o40777)).expect_err("a world-writable directory");
    assert!(err.contains("directory"), "{err}");
}

/// …unless it is sticky, which is exactly what the sticky bit prevents, and
/// `/tmp` is the everyday case. refusing that too would block legitimate cases
/// for nothing.
#[test]
fn the_sticky_bit_makes_a_shared_directory_acceptable() {
    assert!(trust_verdict(0, 0o100600, Some(0o41777)).is_ok());
}
