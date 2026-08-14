//! [`InstallWkhtmltopdf`]: installs the Qt-patched wkhtmltopdf build from
//! GitHub releases, **verifying its SHA-256** before installing (gap G3, which
//! the original Bash did not do).
//!
//! the distribution's own package, without the Qt patch, produces broken PDFs
//! with Odoo.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::checks::OsInfo;
use crate::context::Context;
use crate::distro::OsFamily;
use crate::error::StepError;
use crate::state::PreState;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{self, Downloader, SystemOps};

/// the pinned version: the GitHub release tag.
const WK_VERSION: &str = "0.12.6.1-3";
/// what `wkhtmltopdf --version` reports when it is the right one.
const WK_INSTALLED_MARKER: &str = "0.12.6.1";
/// the package name the undo purges.
const WK_PACKAGE: &str = "wkhtmltox";

/// the package suffix chosen for this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMapping {
    pub suffix: String,
    /// `true` when the system is unmapped and its family's fallback applies.
    pub fallback: bool,
}

/// picks the wkhtmltopdf package suffix, with a **per-family fallback** (A5.2).
///
/// the key changes nature between families: on Debian it is the **codename**,
/// which is how upstream names its packages, while on Fedora the codename does
/// not exist — `VERSION_CODENAME` is empty — and the key is the **version**.
/// hence the whole `OsInfo` rather than two strings.
///
/// the fallback follows the family and not one default: every unknown codename
/// used to fall back to an *Ubuntu* package, and Debian 13 passes the version
/// check — the thresholds are open upwards — so it would have taken a package
/// built for another distribution. a fallback that ignores the relevant
/// dimension is the wrong choice dressed as a default.
///
/// a fallback and not a refusal (A5.1-bis): refusing without evidence blocks
/// the good case, and the TOFU pin stays fail-closed on the contents
/// regardless.
pub fn map_package_suffix(os: &OsInfo) -> PackageMapping {
    let mapped = |s: &str| PackageMapping {
        suffix: s.to_string(),
        fallback: false,
    };
    let fallback = |s: &str| PackageMapping {
        suffix: s.to_string(),
        fallback: true,
    };

    match os.family {
        OsFamily::Debian => match os.codename.as_deref() {
            // no native package: this one is compatible, mapped explicitly.
            Some("noble") | Some("mantic") | Some("lunar") | Some("jammy") => mapped("jammy"),
            Some("bookworm") => mapped("bookworm"),
            Some("bullseye") => mapped("bullseye"),
            // unknown codename: the newest package of **its** family.
            _ => {
                if os.id == "debian" {
                    fallback("bookworm")
                } else {
                    fallback("jammy")
                }
            }
        },
        OsFamily::Fedora => fedora_suffix(&os.version, mapped, fallback),
    }
}

/// the `.rpm` suffix for a Fedora version.
///
/// the release publishes three x86_64 packages, of which **`fedora37`** is the
/// only one built *for Fedora* rather than a RHEL-like, and the only one this
/// function can produce. pinning the others would pin files no path can
/// download.
///
/// every supported Fedora takes it, because it is **the newest upstream built
/// for this family** — A5.2's rule again. `fallback: true` makes that visible
/// in the log.
///
/// a fallback and not a refusal even at that distance: an `.rpm` declares its
/// own `Requires`, so an incompatible package is **rejected before** being
/// installed, with an error naming the missing dependency. the failure would be
/// loud, not silent, which is what makes the fallback acceptable.
fn fedora_suffix(
    version: &str,
    mapped: impl Fn(&str) -> PackageMapping,
    fallback: impl Fn(&str) -> PackageMapping,
) -> PackageMapping {
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if major == 37 {
        mapped("fedora37")
    } else {
        fallback("fedora37")
    }
}

/// TOFU pin for the `jammy` package.
const PIN_JAMMY: &str = "4f723b2691ad8638a9df960e0421d346d7315083e3583a334f33362280ddba15";
/// TOFU pin for the `bullseye` package.
const PIN_BULLSEYE: &str = "9c687f0c58cf50e01f2a6375d2e34372f8feeec56a84690ea113d298fccadd98";
/// TOFU pin for the `fedora37` `.rpm`.
///
/// the only one in the release built for this family. the other two published
/// packages are absent because `fedora_suffix` never produces them, and a pin
/// for a file no path downloads is a line nobody can verify.
const PIN_FEDORA37: &str = "59782f518e50ed074ef41356452f5229a72e6659c3afc2b352c20c916da63d3f";
/// TOFU pin for the `bookworm` package.
const PIN_BOOKWORM: &str = "98ba0d157b50d36f23bd0dedf4c0aa28c7b0c50fcdcdc54aa5b6bbba81a3941d";

/// the expected SHA-256 checksums, **keyed by package suffix**.
///
/// the key is the suffix of the package we download, **not** the user's OS
/// codename: [`map_codename`] translates one into the other.
///
/// # the nature of the guarantee: TOFU pinning
///
/// the official release publishes **no** checksums or signatures for its
/// packages — only the git tag is GPG-signed — so there is no *upstream*
/// checksum to use.
///
/// the honest decision is **manual TOFU pinning**: these are not official
/// checksums but pins generated once from a trusted source, downloaded over
/// HTTPS from the official release, two of them cross-checked against an
/// independent third party. from then on every download is verified against
/// the pin, which protects against compromised mirrors, corrupted downloads
/// and later alterations.
///
/// ## regenerating the pins
/// ```text
/// for cn in jammy bullseye bookworm; do
///   url="https://github.com/wkhtmltopdf/packaging/releases/download/0.12.6.1-3/wkhtmltox_0.12.6.1-3.${cn}_amd64.deb"
///   echo -n "$cn = "; curl -fsSL "$url" | sha256sum | cut -d' ' -f1
/// done
/// ```
/// update the values **and** this procedure when `WK_VERSION` changes: pins
/// hold for one version only. they are never invented, and the check is never
/// bypassed — a suffix without a pin fails the step.
pub fn default_checksums() -> BTreeMap<String, String> {
    [
        ("jammy", PIN_JAMMY),
        ("bullseye", PIN_BULLSEYE),
        ("bookworm", PIN_BOOKWORM),
        ("fedora37", PIN_FEDORA37),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// the suffixes this family has packages for.
///
/// only used to tell "unknown suffix" from "family never calibrated" in the
/// error message: two situations leading to different actions.
fn known_suffixes_for(family: OsFamily) -> &'static [&'static str] {
    match family {
        OsFamily::Debian => &["jammy", "bullseye", "bookworm"],
        OsFamily::Fedora => &["fedora37"],
    }
}

/// installs wkhtmltopdf with checksum verification, reversibly.
pub struct InstallWkhtmltopdf {
    ops: Box<dyn SystemOps>,
    downloader: Box<dyn Downloader>,
    checksums: BTreeMap<String, String>,
    tmp_dir: PathBuf,
    prestate: PreState,
}

impl InstallWkhtmltopdf {
    /// constructor with injectable dependencies, for the tests.
    pub fn with_parts(
        ops: Box<dyn SystemOps>,
        downloader: Box<dyn Downloader>,
        checksums: BTreeMap<String, String>,
        tmp_dir: PathBuf,
    ) -> Self {
        Self {
            ops,
            downloader,
            checksums,
            tmp_dir,
            prestate: PreState::Untracked,
        }
    }

    /// downloads, **verifies the checksum**, installs. always cleans up the
    /// temporary.
    fn download_verify_install(&self, ctx: &Context) -> Result<(), StepError> {
        // without `os_info` no package can be chosen. that only happens in a
        // rollback from disk, where this is never called — the undo removes and
        // does not download.
        let os = ctx.os_info.as_ref().ok_or_else(|| {
            StepError::Precondition(
                "no OS information available: the wkhtmltopdf package for this system \
                 cannot be chosen"
                    .to_string(),
            )
        })?;

        let mapping = map_package_suffix(os);
        if mapping.fallback {
            warn!(
                os = %os.id,
                version = %os.version,
                codename = ?os.codename,
                package = %mapping.suffix,
                "unmapped system: using the most recent package of the same family. if \
                 wkhtmltopdf misbehaves on this release, this is the first place to look."
            );
        }
        let suffix = &mapping.suffix;

        // G3: no expected checksum means no installation, never bypassed.
        //
        // the message distinguishes two cases leading to different actions: an
        // unknown suffix for a family that has pins is a case to map, while a
        // family with **no pins at all** has never been calibrated.
        let expected = self.checksums.get(suffix).ok_or_else(|| {
            let family_without_pins = !self
                .checksums
                .keys()
                .any(|k| known_suffixes_for(os.family).contains(&k.as_str()));
            if family_without_pins {
                StepError::Precondition(format!(
                    "the wkhtmltopdf TOFU pins for family '{}' have not been generated yet \
                     (G3): the installation stops rather than download an unverifiable \
                     binary. the procedure to generate them is documented on \
                     `default_checksums`.",
                    os.family
                ))
            } else {
                StepError::Precondition(format!(
                    "wkhtmltopdf checksum unavailable for '{suffix}' (G3): integrity \
                     cannot be verified, installation refused"
                ))
            }
        })?;

        let pkg = self.ops.packages().local_package_name(WK_VERSION, suffix);
        let url = format!(
            "https://github.com/wkhtmltopdf/packaging/releases/download/{WK_VERSION}/{pkg}"
        );
        // an **unpredictable** name (A-V3-3): with a fixed, known one a local
        // user could plant a symlink there and have root write through it
        // before the installer even started.
        //
        // the *contents* were already defended by the TOFU pin; this closes the
        // other half, where the file is born. the extension must be preserved:
        // the manager recognises a local path only by it.
        let tmp = system_ops::private_temp_path_keeping_extension(&self.tmp_dir, &pkg);

        // download → verify → install; the temporary is cleaned up either way.
        let outcome = self.download_verify_install_inner(&url, &tmp, expected);
        if let Err(e) = std::fs::remove_file(&tmp) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(tmp = %tmp.display(), error = %e, "cleaning up the temporary file failed");
            }
        }
        outcome
    }

    fn download_verify_install_inner(
        &self,
        url: &str,
        tmp: &Path,
        expected: &str,
    ) -> Result<(), StepError> {
        info!(url, tmp = %tmp.display(), "download wkhtmltopdf");
        self.downloader.download(url, tmp)?;

        let actual = system_ops::sha256_hex(tmp)?;
        if actual != *expected {
            // G3: checksum mismatch means NO installation.
            return Err(StepError::Precondition(format!(
                "invalid wkhtmltopdf checksum (G3): expected {expected}, computed {actual}. \
                 installation aborted."
            )));
        }
        info!("wkhtmltopdf checksum verified");

        // installing the local file through the manager resolves the package's
        // system dependencies, which a minimal VM lacks.
        //
        // this used to be `dpkg -i` followed by a fix-up: `dpkg -i` does not
        // resolve dependencies, exits 1 leaving the package unconfigured, and
        // the `?` propagated the error *before* the fix-up ever ran. worse,
        // dpkg stayed broken and every later apt command failed, rollback
        // included (A-RT-1, A-RT-2).
        //
        // integrity still holds: the manager installs **this** file, just
        // verified against the pin.
        self.ops.packages().install_local_file(tmp)?;
        Ok(())
    }
}

impl Step for InstallWkhtmltopdf {
    fn name(&self) -> &str {
        "install-wkhtmltopdf"
    }

    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        // idempotence: the right version already present is not ours.
        self.prestate = match self.ops.wkhtmltopdf_version() {
            Some(version) if version.starts_with(WK_INSTALLED_MARKER) => PreState::Preexisting,
            _ => PreState::Untracked,
        };
        info!(prestate = ?self.prestate, "snapshot: install-wkhtmltopdf");
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate == PreState::Preexisting {
            info!("run: wkhtmltopdf {WK_INSTALLED_MARKER} already present, skipping");
            return Ok(());
        }
        if ctx.dry_run {
            info!("run (dry run): would download, verify the checksum and install wkhtmltopdf");
            return Ok(());
        }

        self.download_verify_install(ctx)?;
        self.prestate = PreState::CreatedByUs;
        info!("run: wkhtmltopdf installed");
        Ok(())
    }

    /// removes **only** `wkhtmltox`, not its system dependencies.
    ///
    /// installing pulls in fonts and libraries: system packages, not Odoo
    /// artifacts. they stay, for the same reason the rollback leaves the
    /// bootstrap utilities — low-risk common packages that something else may
    /// have adopted meanwhile.
    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        if self.prestate != PreState::CreatedByUs {
            info!(prestate = ?self.prestate, "undo NO-OP (wkhtmltopdf not installed by us)");
            return Ok(());
        }
        if ctx.dry_run {
            info!("undo (dry run): purge {WK_PACKAGE}");
            return Ok(());
        }
        crate::steps::remove_with_recovery(
            self.ops.packages(),
            "install-wkhtmltopdf",
            &[WK_PACKAGE],
        );
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
