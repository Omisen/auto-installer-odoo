//! the **dnf/rpm** backend: the Fedora family's commands and names.
//!
//! the **commands** are documented and stable. the **package names** are a
//! translation of the Debian list, and roughly 26 of 30 differ — a wrong one
//! here **stops the installation in the snapshot**, before mutating, naming the
//! unresolved group.
//!
//! calibrating needs no installation: `--dry-run` runs every step's snapshot
//! without touching anything, and reports **all** unresolvable groups in one
//! message.
//!
//! verified on Fedora 41 (dnf5 5.2.17): all 31 groups resolve, and three names
//! turned out to be **virtual** — `wget`, `zlib-devel`, `openjpeg2-devel` — and
//! were corrected with the real name as the preferred alternative, the same
//! care as `libfreetype6-dev` on Ubuntu 24.04. the integration CI then
//! exercised the full install and rollback cycle on `fedora:41` and
//! `fedora:44`.

use std::path::Path;

use super::{
    availability_from, AlternatePython, Availability, CatalogEntry, DepId, PackageCatalog,
    PackageManager,
};
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// bootstrap prerequisites on the Fedora family.
fn bootstrap_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        // on Fedora 41 `wget` is **not a package**: it is provided by
        // `wget1-wget` and `wget2-wget`. the real name goes first.
        CatalogEntry::new(DepId::Wget, &["wget1-wget", "wget2-wget", "wget"]),
        // Debian splits out the runtime part; Fedora ships one `gettext`.
        CatalogEntry::new(DepId::Gettext, &["gettext"]),
    ]
}

/// Odoo's system dependencies on the Fedora family.
///
/// translated from the Debian list and calibrated on a real Fedora — see the
/// module docs.
fn odoo_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        // three alternatives because `wget` here is **purely virtual**: `rpm
        // -q` does not know it and `dnf remove` would exit 0 having removed
        // nothing, so a delta containing it lies (A5.1-bis).
        //
        // `wget1-wget` first, because its `-q -O` options are the ones
        // `RealDownloader` uses; `wget2-wget` as fallback. the virtual name
        // stays last as a net against a future rename.
        CatalogEntry::new(DepId::Wget, &["wget1-wget", "wget2-wget", "wget"]),
        CatalogEntry::new(DepId::PythonPip, &["python3-pip"]),
        CatalogEntry::new(DepId::PythonDev, &["python3-devel"]),
        // there is **no** `python3-venv` here: the module is in the stdlib and
        // `ensurepip` ships in `python3-libs`, already present wherever python3
        // is. the entry stays because the need does, and it resolves to
        // already-installed without padding the delta.
        //
        // the real check is `create-virtualenv`'s precondition, which asks the
        // interpreter for `import ensurepip` (A-R6-1).
        CatalogEntry::new(DepId::PythonVenv, &["python3-libs"]),
        CatalogEntry::new(DepId::PythonWheel, &["python3-wheel"]),
        CatalogEntry::new(DepId::PythonSetuptools, &["python3-setuptools"]),
        // `build-essential` is a Debian metapackage with no equivalent. the
        // `@development-tools` group has its own syntax and unclear removal
        // behaviour, so the delta would not know what to reclaim: three
        // explicit names are what pip's native extensions actually need.
        CatalogEntry::many(DepId::BuildTools, &["gcc", "gcc-c++", "make"]),
        CatalogEntry::new(DepId::Gettext, &["gettext"]),
        CatalogEntry::new(DepId::Freetype, &["freetype-devel"]),
        CatalogEntry::new(DepId::Xml2, &["libxml2-devel"]),
        CatalogEntry::new(DepId::Zip, &["libzip-devel"]),
        // a wholly different name: the library is OpenLDAP.
        CatalogEntry::new(DepId::Ldap, &["openldap-devel"]),
        // likewise: the SASL implementation is Cyrus.
        CatalogEntry::new(DepId::Sasl, &["cyrus-sasl-devel"]),
        // Debian's three jpeg names collapse into one.
        CatalogEntry::new(DepId::Jpeg, &["libjpeg-turbo-devel"]),
        // the same package as `Jpeg`: A-MD-1's deduplication absorbs it, and
        // here the duplicate is the norm rather than an edge case.
        CatalogEntry::new(DepId::Jpeg8, &["libjpeg-turbo-devel"]),
        // the soname drops — and that is not enough: `zlib-devel` is itself
        // **virtual** since the distribution moved to `zlib-ng`, so the real
        // package is `zlib-ng-compat-devel`.
        CatalogEntry::new(DepId::Zlib, &["zlib-ng-compat-devel", "zlib-devel"]),
        CatalogEntry::new(DepId::PostgresClient, &["libpq-devel"]),
        // the `1` drops too.
        CatalogEntry::new(DepId::Xslt, &["libxslt-devel"]),
        CatalogEntry::new(DepId::Tiff, &["libtiff-devel"]),
        // the `2` is history: the real package is `openjpeg-devel`, which
        // provides `openjpeg2-devel` for compatibility.
        CatalogEntry::new(DepId::OpenJpeg, &["openjpeg-devel", "openjpeg2-devel"]),
        CatalogEntry::new(DepId::Lcms2, &["lcms2-devel"]),
        CatalogEntry::new(DepId::Webp, &["libwebp-devel"]),
        CatalogEntry::new(DepId::Harfbuzz, &["harfbuzz-devel"]),
        CatalogEntry::new(DepId::Fribidi, &["fribidi-devel"]),
        CatalogEntry::new(DepId::Xcb, &["libxcb-devel"]),
        CatalogEntry::new(DepId::Ev, &["libev-devel"]),
        CatalogEntry::new(DepId::CAres, &["c-ares-devel"]),
        // optional as on Debian, for the same reason: modern Odoo uses SCSS.
        // todo: confirm it still exists on recent Fedora — but a missing
        // optional is a warning, not a stop.
        CatalogEntry::optional(DepId::LessCompiler, &["nodejs-less"]),
    ]
}

/// the packages that install the PostgreSQL server here.
///
/// **not** `postgresql`, which is the client alone: the server is a separate
/// package, and installing only the client would give a `systemctl start` that
/// fails without saying why.
pub const POSTGRES_PACKAGES: &[&str] = &["postgresql-server", "postgresql-contrib"];
/// the name to ask "is PostgreSQL installed?" with, here.
///
/// the **server**, not the client: `postgresql` is present on a machine that
/// only has `psql`, and using it as the marker would make the server look
/// already there — hence `Preexisting`, hence no undo.
pub const POSTGRES_MARKER_PACKAGE: &str = "postgresql-server";
/// the nginx package, identical on both families.
pub const NGINX_PACKAGE: &str = "nginx";

/// the alternative Python interpreters Fedora packages, newest first.
///
/// several are kept alongside the system one, with the same name for package
/// and binary. needed from Fedora 43, where the system `python3` moved to 3.14
/// and Odoo 18's pins do not cover it (A-MD-7).
///
/// verified on Fedora 44: installing the interpreter and its headers pulls in
/// its libs, the venv builds without extra packages, the whole Odoo 18
/// `requirements.txt` installs, and `dnf remove` takes back exactly what it
/// added.
///
/// the order is the policy: the **newest covered by the pins**, not the oldest
/// available. a closer interpreter gets security updates longer, and stays
/// inside what the installer really exercises.
pub const ALTERNATE_PYTHONS: &[((u32, u32), &str, &str)] = &[
    ((3, 13), "python3.13", "python3.13-devel"),
    ((3, 12), "python3.12", "python3.12-devel"),
];

/// `dnf install`'s arguments, as a **pure** function.
///
/// extracted because the flag that matters is not checkable otherwise: the code
/// that runs `dnf` only executes on a real Fedora, so dropping
/// `install_weak_deps=False` would be a change no test could see.
pub fn install_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "install".to_string(),
        "-y".to_string(),
        // the counterpart of `--no-install-recommends`: without it dnf pulls in
        // weak dependencies and the delta grows with packages nobody asked for
        // — which the undo would then remove.
        "--setopt=install_weak_deps=False".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// `dnf remove`'s arguments, as a **pure** function.
///
/// see [`DnfBackend::remove`] for why `clean_requirements_on_remove=False` is
/// the condition for the surgical promise to hold here.
pub fn remove_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "remove".to_string(),
        "-y".to_string(),
        "--setopt=clean_requirements_on_remove=False".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// the Fedora family's package manager.
#[derive(Debug, Default)]
pub struct DnfBackend;

/// runs `dnf` non-interactively.
///
/// no `DEBIAN_FRONTEND` equivalent is needed: dnf asks nothing under `-y`, and
/// there is no `needrestart` to silence.
///
/// # no `--` before the names, and that is declared
///
/// on apt the `--` separator is **half** the double defence against argument
/// injection (R1); the other half is the validator demanding an alphanumeric
/// first character. **dnf5 rejects it**: `dnf install -- <pkg>` answers
/// `Unknown argument "--"` and exits 2.
///
/// so one defence remains here. the real surface is nil — the names come from
/// the catalogue, which is constants in the source — but an external constraint
/// that weakens a defence is written down, not left to be discovered.
fn run_dnf(args: &[&str]) -> Result<(), StepError> {
    run_command("dnf", args)
}

impl PackageManager for DnfBackend {
    fn is_installed(&self, pkg: &str) -> bool {
        // `rpm` accepts `--`, unlike dnf5: the double defence holds here.
        std::process::Command::new("rpm")
            .args(["-q", "--", pkg])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// `dnf makecache`.
    ///
    /// **less necessary** than on apt, since metadata expire on their own and
    /// are refetched by the first operation that needs them. run anyway for
    /// A5.1-bis's reason: a stale index answers "this package does not exist"
    /// to a question whose answer is "I do not know".
    fn refresh_index(&self) -> Result<(), StepError> {
        run_dnf(&["makecache"])
    }

    /// is there at least one enabled repository to get answers from?
    ///
    /// the analogue of `apt-cache stats`, telling **blindness** from
    /// **absence**. here the concrete case is unreachable repositories rather
    /// than a stale index, with the same effect on the diagnosis.
    fn index_is_queryable(&self) -> bool {
        capture_command("dnf", &["repolist", "--enabled", "--quiet"])
            .map(|out| out.lines().any(|line| !line.trim().is_empty()))
            .unwrap_or(false)
    }

    /// two questions, as on apt, asked of `dnf repoquery` — the command meant
    /// for scripting.
    ///
    /// 1. `repoquery --qf '%{name}'`: is there a package under *this exact
    ///    name*? the equivalent of a real candidate, i.e. one `rpm -q` will
    ///    know afterwards and the undo will be able to remove.
    /// 2. `repoquery --whatprovides`: if not, does something *provide* it?
    ///
    /// # why not the obvious commands
    ///
    /// the first attempt used `dnf list` and `dnf install --assumeno`, and
    /// **neither worked**: dnf5 rejects the `--` separator, and `--assumeno`
    /// exits 2 **even when the package exists**, because the operation was
    /// cancelled — precisely what it was asked to do.
    ///
    /// the second is this project's recurring defect mirrored: not a check that
    /// cannot fail, but one **that cannot succeed**. every package not already
    /// installed came out absent, and the first Fedora dry run stopped listing
    /// twenty-four names that all existed.
    ///
    /// `repoquery` exits **0** either way and answers through its output, not
    /// its exit code — R9-hotfix's lesson: *`exit != 0` does not say WHY*.
    fn availability(&self, pkg: &str) -> Availability {
        let real = capture_command("dnf", &["repoquery", "--quiet", "--qf", "%{name}\n", pkg])
            .map(|out| out.lines().any(|line| line.trim() == pkg))
            .unwrap_or(false);

        // the slow path only when needed, as on apt.
        let provided_by_others = !real
            && capture_command("dnf", &["repoquery", "--quiet", "--whatprovides", pkg])
                .map(|out| out.lines().any(|line| !line.trim().is_empty()))
                .unwrap_or(false);

        availability_from(real, provided_by_others)
    }

    /// `dnf install -y`, **without weak dependencies**.
    ///
    /// the counterpart of `--no-install-recommends`: without it the delta grows
    /// with packages nobody asked for, which the undo would then remove.
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = install_args(pkgs);
        run_dnf(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// `dnf remove -y`, **leaving orphans alone**.
    ///
    /// `clean_requirements_on_remove=False` is the condition for the surgical
    /// promise to hold here. dnf's default is to remove newly useless
    /// dependencies too, which would be exactly the global `autoremove` R0
    /// **banned** from the undo for not being bounded by our delta. on apt that
    /// is an explicit action confined to `--aggressive-rollback`; here it would
    /// happen on every rollback, and could take away a library shared with the
    /// customer's software.
    ///
    /// the flag is passed **always**, even should the default change: a
    /// behaviour a promise rests on is not left to a config file we do not
    /// control.
    ///
    /// # what the flag does NOT prevent
    ///
    /// **reverse dependencies**. removing a package announces the removal of
    /// whatever depended on it, and that is mandatory: rpm cannot leave
    /// installed a package whose dependency disappears. `apt-get purge` does
    /// the same, so it is **not** a divergence between families — but it is a
    /// limit of the surgical promise that holds for both, and must be said.
    ///
    /// # what is left behind, declared
    ///
    /// rpm has no deb-style `purge`: a **modified** config file is renamed to
    /// `.rpmsave` rather than deleted. Odoo's heavy delta is `-devel` packages
    /// with no config files, so the expected residue is **none**; the ones
    /// removed only under `--aggressive-rollback` may leave some. inert and
    /// traceable, like the installer's own log.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = remove_args(pkgs);
        run_dnf(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn remove_orphans(&self) -> Result<(), StepError> {
        run_dnf(&["autoremove", "-y"])
    }

    /// **a no-op**, and not out of laziness.
    ///
    /// `try_repair` exists because apt refuses to operate on a half-finished
    /// `dpkg` (A-RT-2). rpm has no such state: a transaction is applied or
    /// rolled back. the nearest equivalents do different things — one is a
    /// second rollback semantics beside ours, the other rebuilds the database.
    ///
    /// so the recovery policy degrades to "try, retry once, then list the
    /// leftovers" — and the part that matters, the **leftovers report**, is
    /// identical across families.
    fn try_repair(&self) -> Result<(), StepError> {
        Ok(())
    }

    /// a no-op, for the same reason as [`Self::try_repair`].
    fn try_deep_repair(&self) -> Result<(), StepError> {
        Ok(())
    }

    /// `dnf install -y <path.rpm>`: installs a local package, resolving its
    /// dependencies.
    fn install_local_file(&self, path: &Path) -> Result<(), StepError> {
        let rendered = path.to_string_lossy();
        run_dnf(&["install", "-y", &rendered])
    }

    /// upstream's rpm scheme: `wkhtmltox-{ver}.{suffix}.x86_64.rpm`.
    ///
    /// a wrong name would fail the download with a 404 — loud, not silent — and
    /// the TOFU pin for the `fedora37` package is checked before anything is
    /// installed.
    fn local_package_name(&self, version: &str, suffix: &str) -> String {
        format!("wkhtmltox-{version}.{suffix}.x86_64.rpm")
    }

    fn refresh_command(&self) -> &'static str {
        "dnf makecache"
    }

    fn catalog(&self) -> PackageCatalog {
        PackageCatalog {
            bootstrap: bootstrap_catalog(),
            odoo: odoo_catalog(),
            postgres: POSTGRES_PACKAGES.iter().map(|s| s.to_string()).collect(),
            postgres_marker: POSTGRES_MARKER_PACKAGE.to_string(),
            nginx: NGINX_PACKAGE.to_string(),
            alternate_pythons: ALTERNATE_PYTHONS
                .iter()
                .map(|(v, i, d)| AlternatePython::new(*v, i, d))
                .collect(),
        }
    }
}
