//! the **apt/dpkg** backend: the Debian family's commands and names.
//!
//! gathers what used to be spread between `SystemOps` and the constants in
//! `steps::apt_packages`, with no behaviour change: same commands, same
//! arguments, same order.

use std::path::Path;

use super::{availability_from, Availability, CatalogEntry, DepId, PackageCatalog, PackageManager};
use crate::error::StepError;
use crate::system_ops::{
    capture_command_with_env, has_installable_candidate, run_command_with_env, total_package_names,
};

/// runs `apt-get` non-interactively, with no tzdata or needrestart prompts.
///
/// `DEBIAN_FRONTEND` and `NEEDRESTART_MODE` are **Debian-specific**, which is
/// why this lives here and not among the generic `system_ops` helpers.
fn run_apt(args: &[&str]) -> Result<(), StepError> {
    run_command_with_env(
        "apt-get",
        args,
        &[
            ("DEBIAN_FRONTEND", "noninteractive"),
            ("NEEDRESTART_MODE", "a"),
        ],
    )
}

/// does this `apt-get` failure look like the **mirror**, rather than the
/// request?
///
/// pure, and narrow on purpose. a fetch that fails because a mirror reset the
/// connection is worth asking again — it is an ordinary event on a 25-package
/// install, and today it costs a whole installation. a package that does not
/// exist is not worth asking again: retrying a deterministic failure only makes
/// it take three times as long to say the same thing, and hides it behind a
/// wait.
///
/// so the evidence has to name **fetching**. apt says `Failed to fetch` for the
/// umbrella case and the reason after it; a name it cannot resolve gives
/// `Unable to locate package`, which is not here and must not be.
pub fn is_transient_fetch_failure(stderr: &str) -> bool {
    const TRANSIENT: [&str; 6] = [
        "failed to fetch",
        "unable to fetch some archives",
        "connection reset by peer",
        "connection timed out",
        "temporary failure resolving",
        "could not resolve",
    ];
    let lower = stderr.to_lowercase();
    TRANSIENT.iter().any(|marker| lower.contains(marker))
}

/// bootstrap prerequisites: low-risk common utilities.
///
/// these four names are stable across every supported release, so they carry no
/// alternatives.
fn bootstrap_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        CatalogEntry::new(DepId::Wget, &["wget"]),
        CatalogEntry::new(DepId::Gettext, &["gettext-base"]),
    ]
}

/// Odoo's system dependencies on the Debian family.
///
/// each entry may carry alternatives in order of preference, and the first
/// installable name wins. the multi-name groups are the ones that change
/// between releases (A5.1).
fn odoo_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        CatalogEntry::new(DepId::Wget, &["wget"]),
        CatalogEntry::new(DepId::PythonPip, &["python3-pip"]),
        CatalogEntry::new(DepId::PythonDev, &["python3-dev"]),
        CatalogEntry::new(DepId::PythonVenv, &["python3-venv"]),
        CatalogEntry::new(DepId::PythonWheel, &["python3-wheel"]),
        CatalogEntry::new(DepId::PythonSetuptools, &["python3-setuptools"]),
        CatalogEntry::new(DepId::BuildTools, &["build-essential"]),
        CatalogEntry::new(DepId::Gettext, &["gettext-base"]),
        // on Ubuntu 24.04 `libfreetype6-dev` became purely virtual: installable
        // but not purgeable. the real name as an alternative keeps the delta
        // made of things the undo can actually remove (A5.1-bis).
        CatalogEntry::new(DepId::Freetype, &["libfreetype6-dev", "libfreetype-dev"]),
        CatalogEntry::new(DepId::Xml2, &["libxml2-dev"]),
        CatalogEntry::new(DepId::Zip, &["libzip-dev"]),
        CatalogEntry::new(DepId::Ldap, &["libldap2-dev"]),
        CatalogEntry::new(DepId::Sasl, &["libsasl2-dev"]),
        CatalogEntry::new(DepId::Jpeg, &["libjpeg-dev"]),
        CatalogEntry::new(DepId::Zlib, &["zlib1g-dev"]),
        CatalogEntry::new(DepId::PostgresClient, &["libpq-dev"]),
        CatalogEntry::new(DepId::Xslt, &["libxslt1-dev"]),
        // renamed without the soname: 22.04 has both, Debian 12 only the
        // second.
        CatalogEntry::new(DepId::Tiff, &["libtiff5-dev", "libtiff-dev"]),
        // a transitional package on Ubuntu; absent on Debian 12, where
        // `libjpeg-dev` already covers it.
        //
        // A-MD-1: where neither of the first two exists this resolves to the
        // **same name** as `DepId::Jpeg`. resolution deduplicates before
        // composing the delta — the manifest is the accounting of what we
        // added, and a double entry is wrong accounting.
        CatalogEntry::new(
            DepId::Jpeg8,
            &["libjpeg8-dev", "libjpeg-turbo8-dev", "libjpeg-dev"],
        ),
        CatalogEntry::new(DepId::OpenJpeg, &["libopenjp2-7-dev"]),
        CatalogEntry::new(DepId::Lcms2, &["liblcms2-dev"]),
        CatalogEntry::new(DepId::Webp, &["libwebp-dev"]),
        CatalogEntry::new(DepId::Harfbuzz, &["libharfbuzz-dev"]),
        CatalogEntry::new(DepId::Fribidi, &["libfribidi-dev"]),
        CatalogEntry::new(DepId::Xcb, &["libxcb1-dev"]),
        CatalogEntry::new(DepId::Ev, &["libev-dev"]),
        CatalogEntry::new(DepId::CAres, &["libc-ares-dev"]),
        // optional: the `.less` asset compiler. modern Odoo uses SCSS and
        // starts without it, and the package was dropped from some Debian
        // releases — a nice-to-have must not make installation impossible.
        CatalogEntry::optional(DepId::LessCompiler, &["node-less"]),
    ]
}

/// the packages that install the PostgreSQL server here.
pub const POSTGRES_PACKAGES: &[&str] = &["postgresql", "postgresql-contrib"];
/// the name to ask "is PostgreSQL installed?" with.
pub const POSTGRES_MARKER_PACKAGE: &str = "postgresql";
/// the nginx package.
pub const NGINX_PACKAGE: &str = "nginx";

/// `apt-get install`'s arguments, as a **pure** function.
///
/// the code that runs apt only executes on a real machine, and
/// `--no-install-recommends` protects the delta: without it packages nobody
/// asked for enter, and the undo would remove them. as a return value the flag
/// is checkable.
pub fn install_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "install".to_string(),
        "-y".to_string(),
        "--no-install-recommends".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// `apt-get purge`'s arguments, as a **pure** function.
///
/// apt does not remove orphans unless asked, so [`PackageManager::remove`]'s
/// invariant holds with no extra options. dnf differs, and has to disable it
/// explicitly.
pub fn remove_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec!["purge".to_string(), "-y".to_string()];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// the Debian family's package manager.
///
/// stateless: the commands run on every call.
#[derive(Debug, Default)]
pub struct AptBackend;

impl PackageManager for AptBackend {
    fn is_installed(&self, pkg: &str) -> bool {
        match std::process::Command::new("dpkg-query")
            .args(["-W", "-f=${Status}", pkg])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).contains("install ok installed")
            }
            _ => false,
        }
    }

    fn refresh_index(&self) -> Result<(), StepError> {
        run_apt(&["update"])
    }

    fn index_is_queryable(&self) -> bool {
        match capture_command_with_env("apt-cache", &["stats"], &[("LC_ALL", "C")]) {
            Ok(out) => total_package_names(&out).is_some_and(|n| n > 0),
            // we cannot ask: treat the index as unqueryable, which yields a
            // cautious message rather than a verdict of absence.
            Err(_) => false,
        }
    }

    /// two commands, in this order: apt's **mechanism**, not the policy, which
    /// lives in `AptPackagesStep::resolve`.
    ///
    /// 1. `apt-cache policy` is the fast path and covers the normal cases: a
    ///    `Candidate:` other than `(none)` means `dpkg-query` will know this
    ///    name after installation.
    /// 2. otherwise "does not exist" still has to be told from "purely
    ///    virtual", and only the resolver answers that: `apt-get install -s`
    ///    simulates without mutating, and exits 0 even for a single-provider
    ///    `Provides`. slower, so it is the fallback and not the first
    ///    question.
    fn availability(&self, pkg: &str) -> Availability {
        // apt-cache missing or failing: no information, not a verdict. try the
        // slow path, and let the caller cross-check with `index_is_queryable`.
        let policy_says_real =
            capture_command_with_env("apt-cache", &["policy", "--", pkg], &[("LC_ALL", "C")])
                .map(|out| has_installable_candidate(&out))
                .unwrap_or(false);

        // the slow path only when needed: `-s` simulates without touching the
        // system but runs the resolver. exits 100 for a name that does not
        // exist, 0 when installable — virtual single-provider included.
        let resolver_accepts = !policy_says_real
            && run_apt(&["install", "-s", "-y", "--no-install-recommends", "--", pkg]).is_ok();

        availability_from(policy_says_real, resolver_accepts)
    }

    fn is_transient_failure(&self, stderr: &str) -> bool {
        is_transient_fetch_failure(stderr)
    }

    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = install_args(pkgs);
        run_apt(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// `apt-get purge`: removes the named packages **and their config files**,
    /// and nothing else.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = remove_args(pkgs);
        run_apt(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn remove_orphans(&self) -> Result<(), StepError> {
        run_apt(&["autoremove", "-y"])
    }

    /// `apt-get install -f -y`: installs missing dependencies and finishes
    /// configuring half-done packages, bringing `dpkg` back to consistency.
    fn try_repair(&self) -> Result<(), StepError> {
        run_apt(&["install", "-f", "-y"])
    }

    /// `dpkg --configure -a`: reconfigures unpacked-but-unconfigured packages,
    /// for when apt itself refuses to operate.
    fn try_deep_repair(&self) -> Result<(), StepError> {
        run_command_with_env(
            "dpkg",
            &["--configure", "-a"],
            &[
                ("DEBIAN_FRONTEND", "noninteractive"),
                ("NEEDRESTART_MODE", "a"),
            ],
        )
    }

    /// `apt-get install -y <path.deb>`.
    ///
    /// replaces `dpkg -i`, which installs the package but does **not** resolve
    /// dependencies: on a minimal system the package stays `unconfigured`,
    /// `dpkg` errors, and from there every apt command fails — rollback
    /// included (A-RT-1, A-RT-2).
    fn install_local_file(&self, path: &Path) -> Result<(), StepError> {
        // `--` before the path: a package in a directory whose name starts with
        // `-` must not become an option.
        let rendered = path.to_string_lossy();
        run_apt(&["install", "-y", "--", &rendered])
    }

    fn local_package_name(&self, version: &str, suffix: &str) -> String {
        format!("wkhtmltox_{version}.{suffix}_amd64.deb")
    }

    fn refresh_command(&self) -> &'static str {
        "apt-get update"
    }

    fn catalog(&self) -> PackageCatalog {
        PackageCatalog {
            bootstrap: bootstrap_catalog(),
            odoo: odoo_catalog(),
            postgres: POSTGRES_PACKAGES.iter().map(|s| s.to_string()).collect(),
            postgres_marker: POSTGRES_MARKER_PACKAGE.to_string(),
            nginx: NGINX_PACKAGE.to_string(),
            // **empty, and not a gap.** the base repositories package ONE
            // Python; alternative interpreters come from third parties, and
            // adding an external repository is a system mutation of another
            // order — not undoable as cleanly, and a trust that is not ours to
            // grant.
            //
            // nothing real is left uncovered: every supported release's Python
            // is inside Odoo's pins, and the day one is not, the warning
            // applies — tell the truth rather than add repositories quietly.
            alternate_pythons: Vec::new(),
        }
    }
}
