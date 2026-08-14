//! consistency of the configuration the integration CI uses.
//!
//! the `.env` file, the integration script and the `.env` parser must say the
//! same thing. when they diverge the symptom arrives after **forty minutes** of
//! job — or worse, never: a misspelled key is a warning, not an error, and the
//! installer carries on with defaults. the integration test would then check a
//! database nobody created, and pass or fail for the wrong reason.
//!
//! these run in the fast CI, on mocks, in milliseconds.

use std::path::Path;

use invok::config::{self, ResolvedConfig};

const CI_ENV: &str = "configs/ci.env";
const CI_NGINX_ENV: &str = "configs/ci-nginx.env";
const CI_SCRIPT: &str = "scripts/ci/integration-test.sh";

/// the keys the `.env` parser recognises.
///
/// duplicated here on purpose, as a **guard**: the source of truth stays in the
/// parser. a key added to the CI config that the parser would silently ignore
/// surfaces here at once.
const KNOWN_KEYS: &[&str] = &[
    "ODOO_VERSION",
    "ODOO_USER",
    "DB_USER",
    "DB_PASSWORD",
    "ODOO_PORT",
    "DB_NAME",
    "ODOO_INSTALL_DIR",
    "ODOO_ADMIN_PASSWD",
    "ODOO_LOGFILE",
    "WITH_NGINX",
    "NGINX_SERVER_NAME",
    "NGINX_OPEN_HTTPS_PORT",
    // the historical name, still recognised: it lives in customers' files
    // (A-V3-6).
    "NGINX_ENABLE_SSL",
];

fn resolve_ci_env() -> ResolvedConfig {
    let raw = config::parse_env_file(Path::new(CI_ENV)).expect("configs/ci.env must exist");
    let empty = config::RawConfig::default();
    // non-interactive, the way CI runs it. the flag matters: there the weak
    // default password is a hard stop.
    ResolvedConfig::resolve(&empty, &raw, &empty, false)
        .expect("configs/ci.env must resolve without any interactive input")
}

#[test]
fn every_key_in_ci_env_is_understood_by_the_parser() {
    for file in [CI_ENV, CI_NGINX_ENV] {
        assert_keys_are_known(file);
    }
}

fn assert_keys_are_known(file: &str) {
    let content = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("{file} must exist"));
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line
            .strip_prefix("export ")
            .unwrap_or(line)
            .split_once('=')
            .map(|(k, _)| k.trim())
            .unwrap_or_else(|| panic!("line {} without '=': {line}", lineno + 1));
        assert!(
            KNOWN_KEYS.contains(&key),
            "{file} line {}: key '{key}' is not recognised by the .env parser. it would be \
             ignored with a warning and the installer would use the default, \
             falsifying the integration test.",
            lineno + 1
        );
    }
}

/// B-V3-7: the nginx config differs from the base one **only** in nginx.
///
/// if they diverged elsewhere, a failure appearing only in the nginx job would
/// no longer be attributable to nginx — and that attribution is the whole value
/// of having two identical-but-one jobs.
#[test]
fn the_nginx_ci_config_differs_only_by_nginx() {
    let base = config::parse_env_file(Path::new(CI_ENV)).expect("ci.env");
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");

    assert_eq!(base.version, con_nginx.version);
    assert_eq!(base.odoo_user, con_nginx.odoo_user);
    assert_eq!(base.db_name, con_nginx.db_name);
    assert_eq!(base.db_user, con_nginx.db_user);
    assert_eq!(base.port, con_nginx.port);
    assert_eq!(base.admin_passwd, con_nginx.admin_passwd);
    assert_eq!(base.logfile, con_nginx.logfile);

    assert_eq!(
        base.with_nginx,
        Some(false),
        "the base config stays without nginx"
    );
    assert_eq!(
        con_nginx.with_nginx,
        Some(true),
        "that is the only reason this file exists"
    );
}

/// the flag that opens 443 stays **out** of the CI config, and not by accident:
/// on a runner the firewall is installed but inactive, so the step would exit
/// at once without adding coverage — and the flag does not touch the vhost
/// anyway (A-V3-6).
#[test]
fn the_nginx_ci_config_does_not_ask_for_the_https_port() {
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");
    assert_eq!(con_nginx.open_https_port, None);
}

#[test]
fn ci_env_resolves_to_what_the_integration_script_expects() {
    let cfg = resolve_ci_env();

    // the database name is the test's pivot: it must NOT be the default. were
    // the rollback to take names from defaults instead of the persisted config,
    // it would look for the wrong one and leave ours behind — and the
    // cleanliness check would see it.
    assert_eq!(cfg.db_name, "citest");
    assert_ne!(
        cfg.db_name, "odoo",
        "the CI's db_name must differ from the default, or the test cannot tell \
         'the rollback used the persisted config' from 'it guessed'"
    );

    assert_eq!(cfg.version, "18.0");
    assert_eq!(cfg.version_short, "18");
    assert_eq!(cfg.odoo_user, "odoo");
    assert_eq!(cfg.db_user, "odoo");
    assert_eq!(cfg.port, 8069);
    assert!(!cfg.with_nginx, "the CI probe does not configure nginx");
    assert!(
        cfg.odoo_logfile.is_none(),
        "an empty ODOO_LOGFILE means logging to the journal: no log dir to check"
    );
    assert!(
        cfg.db_password.is_empty(),
        "an empty password means peer authentication, the path we want to exercise"
    );
}

#[test]
fn the_ci_admin_password_is_not_the_weak_default() {
    // non-interactively the resolution refuses the weak default password, so
    // with that value the CI would not even start. the helper above already
    // proves it; this says why, so whoever edits the file knows what they are
    // touching.
    let cfg = resolve_ci_env();
    assert_ne!(
        cfg.admin_passwd.expose(),
        "admin",
        "an 'admin' password fails a non-interactive installation before any step"
    );
}

#[test]
fn the_integration_script_and_ci_env_agree_on_the_artifacts() {
    // the script's defaults must match the file's values: it checks database,
    // user and port *by name*, and a mismatch would make the assertions vacuous
    // — looking for artifacts nobody created — instead of failing.
    let script = std::fs::read_to_string(CI_SCRIPT).expect("scripts/ci/integration-test.sh");
    let cfg = resolve_ci_env();

    for (var, expected) in [
        ("DB_NAME", cfg.db_name.clone()),
        ("DB_ROLE", cfg.db_user.clone()),
        ("OS_USER", cfg.odoo_user.clone()),
        ("PORT", cfg.port.to_string()),
        ("VER_SHORT", cfg.version_short.clone()),
    ] {
        let needle = format!("{var}:-{expected}}}");
        assert!(
            script.contains(&needle),
            "the integration script must carry `${{{var}:-{expected}}}` to match \
             configs/ci.env (the expected fragment is `{needle}`)"
        );
    }
}

#[test]
fn the_integration_script_is_executable_and_syntactically_valid() {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(CI_SCRIPT).expect("the script must exist");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "the integration script must carry the execute bit"
    );

    // a syntax-only check runs nothing. a broken script would otherwise surface
    // halfway through a forty-minute job, with half a system already installed.
    let status = std::process::Command::new("bash")
        .args(["-n", CI_SCRIPT])
        .status()
        .expect("bash must be available");
    assert!(status.success(), "invalid syntax in {CI_SCRIPT}");
}

// --- A5.3: the "newest tested release" constants follow the real CI ---------

const WORKFLOW: &str = ".github/workflows/integration.yml";

/// the constants tell the user what the installer is really tested on.
/// diverging from the CI matrix would make the warning lie in one of two
/// directions: silent about an untested release, or alarming about one we do
/// test.
///
/// the workflow stays the source of truth; the constants chase it, and this
/// test makes that mandatory rather than desirable.
#[test]
fn the_newest_tested_releases_match_the_ci_matrix() {
    use invok::checks::{NEWEST_TESTED_DEBIAN, NEWEST_TESTED_FEDORA, NEWEST_TESTED_UBUNTU};

    let wf = std::fs::read_to_string(WORKFLOW).expect("the integration workflow must exist");

    // the matrix entries plus the individual jobs' runners, which do not go
    // through it.
    let ubuntu_max = versions_in(&wf, "ubuntu-")
        .into_iter()
        .max()
        .expect("the CI must run on at least one Ubuntu");
    assert_eq!(
        ubuntu_max, NEWEST_TESTED_UBUNTU,
        "the CI runs on Ubuntu {ubuntu_max:?} while the constant says \
         {NEWEST_TESTED_UBUNTU:?}: the untested-release warning would lie"
    );

    // the container images.
    let debian_max = versions_in(&wf, "debian:")
        .into_iter()
        .max()
        .expect("the CI must run on at least one Debian");
    assert_eq!(
        debian_max, NEWEST_TESTED_DEBIAN,
        "the CI runs on Debian {debian_max:?} while the constant says {NEWEST_TESTED_DEBIAN:?}"
    );

    // here the matrix says TWO different things: blocking entries, where a red
    // stops everything, and a PROBE on a never-supported release, tolerated red
    // because an expected red teaches people to ignore reds.
    //
    // the constant must follow the blocking entries only: the warning promises
    // "releases the installer is tested on", and a release whose failure stops
    // nobody is observed, not tested.
    let fedora_blocking = versions_in(&without_probes(&wf), "fedora:");
    let fedora_max = fedora_blocking
        .into_iter()
        .max()
        .expect("the CI must run on at least one blocking Fedora");
    assert_eq!(
        fedora_max, NEWEST_TESTED_FEDORA,
        "the CI runs on Fedora {fedora_max:?} while the constant says {NEWEST_TESTED_FEDORA:?}"
    );

    // and the marker must not become a way to silence the guard: a probe only
    // makes sense on a release NEWER than the tested ones. marking a blocking
    // entry as a probe would remove it from the comparison above with nothing
    // to say so — the same defect this test exists to prevent, one level up.
    for probe in versions_in(&probes_only(&wf), "fedora:") {
        assert!(
            probe > NEWEST_TESTED_FEDORA,
            "the Fedora probe {probe:?} is not newer than {NEWEST_TESTED_FEDORA:?}: either it is a \
             blocking entry mistakenly marked as a probe, or the constant has fallen behind \
             a release that is genuinely tested now"
        );
    }
}

/// the workflow without the lines marked as a non-blocking probe.
///
/// textual, like the rest of this guard: the file is read as it is, without a
/// YAML parser for a question one line answers.
fn without_probes(wf: &str) -> String {
    wf.lines()
        .filter(|line| !line.contains(PROBE_MARKER))
        .collect::<Vec<_>>()
        .join("\n")
}

/// only the lines marked as a probe.
fn probes_only(wf: &str) -> String {
    wf.lines()
        .filter(|line| line.contains(PROBE_MARKER))
        .collect::<Vec<_>>()
        .join("\n")
}

/// the comment marking a matrix entry as a probe tolerated in red. must match
/// the workflow.
const PROBE_MARKER: &str = "non-blocking-probe";

/// every version following `prefix` in the text, as `(major, minor)`.
fn versions_in(text: &str, prefix: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (_, rest) in text
        .match_indices(prefix)
        .map(|(i, m)| (i, &text[i + m.len()..]))
    {
        let number: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if number.is_empty() {
            continue;
        }
        let mut parts = number.split('.');
        let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        out.push((major, minor));
    }
    out
}

// --- M5: the two packages must contain the same thing -----------------------

/// both packages place **the same binary in the same place**.
///
/// the two tools do not talk to each other and read different metadata blocks:
/// the asset list is written twice, in two syntaxes, in the same file. two
/// lists that must coincide with nobody checking is how one ends up publishing
/// a package without the binary in it, unnoticed until a user tries to install
/// it.
///
/// the packages' *contents* are not checked — that would mean building them —
/// only that the two declarations promise the same thing.
#[test]
fn the_deb_and_the_rpm_ship_the_same_binary() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");

    let block = |heading: &str| -> String {
        let start = manifest
            .find(heading)
            .unwrap_or_else(|| panic!("the {heading} block is missing"));
        let rest = &manifest[start + heading.len()..];
        let end = rest.find("\n[").unwrap_or(rest.len());
        rest[..end].to_string()
    };

    let deb = block("[package.metadata.deb]");
    let rpm = block("[package.metadata.generate-rpm]");

    for (name, text, destination) in [
        // one tool: ["source", "destination/", "mode"]
        ("deb", &deb, "usr/bin/"),
        // the other: { source, dest, mode }
        ("rpm", &rpm, "/usr/bin/invok"),
    ] {
        assert!(
            text.contains("target/release/invok"),
            "{name}: it must package the compiled binary and nothing else"
        );
        assert!(
            text.contains(destination),
            "{name}: the binary must land in /usr/bin, or the command is not on the PATH"
        );
        assert!(
            text.contains("755"),
            "{name}: a binary without the execute bit is not a binary"
        );
        // the SOURCE, not just "README": the destination is
        // `usr/share/doc/invok/README`, so a bare substring check passes even
        // when the wrong file is shipped. what must be pinned is that the
        // packages carry the plain-text `PACKAGE-README` and never `README.md`,
        // whose install section is markdown-and-HTML for a renderer that does
        // not exist at `less /usr/share/doc/invok/README`.
        assert!(
            text.contains("PACKAGE-README"),
            "{name}: the packages must ship PACKAGE-README, the plain-text doc"
        );
        assert!(
            !text.contains("\"README.md\"") && !text.contains("= \"README.md\""),
            "{name}: README.md is the repository's front page, not the packaged doc"
        );
    }

    // the promise both make: they package the TOOL, not Odoo. no service, no
    // dependency on the databases or the proxy — the installer creates those at
    // runtime, which is what makes the package harmless to install.
    //
    // the **declaration lines** are inspected, not the whole text: the
    // description names those programs rightly, and a check searching the words
    // anywhere would fail on a correct sentence. this test's first version did
    // exactly that.
    let declarations = |text: &str, keys: &[&str]| -> String {
        text.lines()
            .filter(|line| {
                let r = line.trim_start();
                keys.iter().any(|k| r.starts_with(k))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    for (name, text) in [("deb", &deb), ("rpm", &rpm)] {
        assert!(
            !text.contains("systemd-units"),
            "{name}: the package installs no services"
        );
        let deps = declarations(text, &["depends", "requires", "recommends", "suggests"]);
        assert!(
            !deps.contains("postgresql") && !deps.contains("nginx") && !deps.contains("python"),
            "{name}: no dependency on what the installer handles at runtime, found: {deps}"
        );
    }
}

/// the README's install commands point at **this package's** version, not a
/// past one.
///
/// the commands are whole strings, copyable without reading — the right shape
/// for whoever installs, and **eleven** places where a version can fall behind.
/// two sources that must coincide with nobody checking is how customers end up
/// downloading the previous release in silence: the command works, the file
/// exists, and nothing signals it.
///
/// the correspondence is with the manifest, the version the release workflow
/// will tag, so the README is current when the manifest is and not "when
/// somebody remembers".
///
/// **the release number was not the only way to get the filename wrong**
/// (A-V3-17): one release had the right version everywhere and an unreachable
/// package, because the name lacked the revision the packaging tool adds. hence
/// composed names instead of hand-written ones.
#[test]
fn the_readme_download_commands_point_at_this_version() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml must declare a version");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // every download URL names THIS version.
    let mut url = 0;
    for piece in readme.split("releases/download/v").skip(1) {
        url += 1;
        let found = piece.split('/').next().unwrap_or("");
        assert_eq!(
            found, version,
            "the README downloads from release v{found} while the package is {version}"
        );
    }
    assert!(
        url >= 6,
        "attesi almeno sei link di download (tar.gz, .deb, .rpm, con i rispettivi .sha256), \
         found {url}: if the section changed, this guard must be updated with it"
    );

    // and the filenames, composed with the REVISION declared in the manifest —
    // not one written by hand here (A-V3-17).
    for expected in [package_file_name("deb"), package_file_name("rpm")] {
        assert!(
            readme.contains(&expected),
            "the README does not name `{expected}`: the install command would download a \
             file that release does not contain"
        );
    }
}

/// the package revision is **declared** in the manifest, not inherited.
///
/// **A-V3-17.** one release's package carried a revision suffix the README's
/// name lacked, so following the install command gave a 404. the suffix was
/// there because the packaging tool adds one by default — the artifact's name,
/// which the README promises in full, was decided **outside the repository** by
/// a default that can change between tool versions.
///
/// declaring it is what lets the guard above say no. while the expected name
/// was a hand-written string, the test and the README repeated the same
/// conjecture and neither read the tool: a check that cannot fail in the
/// scenario it exists for.
#[test]
fn the_package_revision_is_declared_not_inherited() {
    for (section, key) in [
        ("[package.metadata.deb]", "revision"),
        ("[package.metadata.generate-rpm]", "release"),
    ] {
        assert!(
            manifest_value(section, key).is_some(),
            "{section} must declare `{key}`: without it the published filename is decided by \
             the tool's default, and the README promises a name nobody checks"
        );
    }
}

/// the value of `key` inside `section` of the manifest, if present.
///
/// parsed by section and not by line: a bare key name also appears in a profile
/// block, and reading the right key in the wrong section is how a guard looks
/// like it works while measuring something else.
fn manifest_value(section: &str, key: &str) -> Option<String> {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");
    let start = manifest.find(section)? + section.len();
    let rest = &manifest[start..];
    let block = &rest[..rest.find("\n[").unwrap_or(rest.len())];
    block.lines().find_map(|line| {
        line.trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?
            .trim()
            .trim_matches('"')
            .to_string()
            .into()
    })
}

/// the filename that packaging produces, composed from the manifest.
///
/// the name's **shape** stays written here — it is the tools' convention, not a
/// datum the repository owns — while version and revision are read. that leaves
/// a residue of conjecture, and it is declared: checking against the file
/// **actually produced** is the release workflow's job, which holds the package
/// before publishing it. this is the fast guard; that one is the last word.
fn package_file_name(confezione: &str) -> String {
    let version = manifest_value("[package]", "version").expect("version in the manifest");
    match confezione {
        "deb" => {
            let rev = manifest_value("[package.metadata.deb]", "revision")
                .expect("the .deb revision in the manifest");
            format!("invok_{version}-{rev}_amd64.deb")
        }
        _ => {
            let rel = manifest_value("[package.metadata.generate-rpm]", "release")
                .expect("the .rpm release in the manifest");
            format!("invok-{version}-{rel}.x86_64.rpm")
        }
    }
}

/// the version the binary declares is the manifest's.
///
/// it comes from the compile-time environment, so today it cannot diverge — the
/// test exists for the day somebody turns it into a hand-written constant to
/// "make it configurable". the README's guard applied to the version's third
/// consumer: flag, log and manifest must all say the same number (A-V3-16).
#[test]
fn the_version_the_binary_reports_is_the_one_in_the_manifest() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");
    let declared = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml must declare a version");

    assert_eq!(
        invok::INSTALLER_VERSION,
        declared,
        "the binary claims to be {} while the package is {declared}",
        invok::INSTALLER_VERSION
    );
}

// --- Non-affiliation: the same promise on three faces ----------------------

/// The non-affiliation disclaimer is present **wherever the package shows its
/// face**: README, `.deb` and `.rpm`.
///
/// Not a formality: the whole point of the trademark question is that nobody
/// mistakes this tool for a product of Odoo S.A., and whoever installs from
/// `apt`/`dnf` never opens the README — they read `apt show` / `dnf info`. A
/// disclaimer living only in the README protects exactly the reader who did not
/// need it.
///
/// The three sentences differ by necessity (the `.rpm` has no long-description
/// field: `cargo-generate-rpm` exposes only `summary`), so their texts are not
/// compared with each other — each is required to **name Odoo S.A. and deny the
/// affiliation**. That is the minimum that makes the promise checkable without
/// freezing the wording.
#[test]
fn every_package_face_disclaims_affiliation_with_odoo() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");
    let readme = std::fs::read_to_string("README.md").expect("reading README.md");
    let packaged = std::fs::read_to_string("PACKAGE-README").expect("reading PACKAGE-README");

    // The metadata block, isolated: searching the whole manifest would pass
    // even if the disclaimer lived only in a comment — and a comment ends up in
    // no package.
    let block = |heading: &str| -> String {
        let start = manifest
            .find(heading)
            .unwrap_or_else(|| panic!("the {heading} block is missing"));
        let rest = &manifest[start + heading.len()..];
        let end = rest.find("\n[").unwrap_or(rest.len());
        // Comments out: the disclaimer must live in a VALUE.
        rest[..end]
            .lines()
            .filter(|r| !r.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    };

    for (where_, text) in [
        ("README.md", readme.as_str()),
        // the fourth face: the only one a customer reads AFTER installing, and
        // the one a split into two documents could quietly leave behind.
        ("PACKAGE-README", packaged.as_str()),
        (
            ".deb (extended-description)",
            &block("[package.metadata.deb]"),
        ),
        (".rpm (summary)", &block("[package.metadata.generate-rpm]")),
    ] {
        assert!(
            text.contains("Odoo S.A."),
            "{where_}: the disclaimer must name the trademark holder"
        );
        // Case-insensitive: the sentence opens a paragraph in one place and
        // sits mid-sentence in another, and freezing the capital letter would
        // fail on a correct text.
        assert!(
            text.to_lowercase()
                .contains("not affiliated with odoo s.a."),
            "{where_}: the explicit denial of affiliation is missing"
        );
    }
}

// --- both packages install the SAME alias -----------------------------------

/// both packages create the same short alias, with the same two cautions.
///
/// the maintainer scripts are four separate files because the **guards** must
/// diverge: each packaging convention passes different arguments, and that part
/// cannot be unified.
///
/// what must **not** diverge is the ACTION, and that is what is checked here:
/// same link, same target, and the two cautions — do not overwrite somebody
/// else's file, do not remove a link repointed elsewhere. without this test
/// there would be two copies of one logic in two formats, which is how one of
/// them falls behind.
#[test]
fn the_deb_and_the_rpm_install_the_same_alias() {
    let read_file =
        |p: &str| std::fs::read_to_string(p).unwrap_or_else(|_| panic!("{p} is missing"));

    let installers = [
        ("deb", read_file("debian/postinst")),
        ("rpm", read_file("rpm/post.sh")),
    ];
    for (wrapper, script) in &installers {
        assert!(
            script.contains("ln -sfn invok /usr/bin/vok"),
            "{wrapper}: the alias must be a RELATIVE symlink to `invok`"
        );
        // the caution: a target that is not a symlink belongs to somebody else
        // and is not overwritten.
        assert!(
            script.contains("[ ! -L /usr/bin/vok ]"),
            "{wrapper}: it must not overwrite a /usr/bin/vok that is not a link"
        );
    }

    let removers = [
        ("deb", read_file("debian/postrm")),
        ("rpm", read_file("rpm/postun.sh")),
    ];
    for (wrapper, script) in &removers {
        // only OUR link is removed: pointing elsewhere means it is not ours.
        assert!(
            script.contains(r#"[ "$(readlink /usr/bin/vok)" = "invok" ]"#),
            "{wrapper}: it must remove only a link still pointing at invok"
        );
    }
    // and only on a real removal, never during an upgrade — written in the two
    // different conventions, which is exactly why there are two files.
    assert!(
        removers[0].1.contains("remove | purge") || removers[0].1.contains("remove|purge"),
        "deb: the alias must not be removed on an `upgrade`"
    );
    assert!(
        removers[1].1.contains(r#"[ "$1" = "0" ]"#),
        "rpm: the alias must be removed only when $1 is 0 (a real uninstall)"
    );

    // the declared paths must EXIST: the tool accepts either an inline script
    // or a path and tells them apart by whether the file is there. a wrong path
    // is no error — it lands in the package as a literal command, and shows up
    // on a customer's machine.
    let manifest = std::fs::read_to_string("Cargo.toml").expect("reading Cargo.toml");
    for field in ["post_install_script", "post_uninstall_script"] {
        let path_value = manifest
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{field} = ")))
            .map(|v| v.trim().trim_matches('"'))
            .unwrap_or_else(|| panic!("Cargo.toml must declare {field}"));
        assert!(
            Path::new(path_value).is_file(),
            "{field} points at `{path_value}`, which is not a file: it would land in the .rpm \
             as a literal command instead of a scriptlet"
        );
    }
}

// --- crates.io: the fourth package, and the only irreversible one -----------

/// the metadata the registry demands is present and within its limits.
///
/// those limits are enforced by the **registry**, not by the compiler, so a
/// violation shows neither when building nor when testing: it shows when the
/// publish is refused, after the tag is pushed and the release published. the
/// worst moment, for the one channel with no undo.
///
/// the category list is NOT checked here: it is closed and lives on the
/// registry, and copying it would mean keeping a fourth copy of data we do not
/// own aligned. the dry-run publish in the release workflow says no on that, by
/// asking the real registry. here only what the repository knows about itself.
#[test]
fn the_crate_metadata_is_publishable() {
    let read_key = |key: &str| {
        manifest_value("[package]", key)
            .unwrap_or_else(|| panic!("[package] must declare `{key}` to publish"))
    };

    // the readme key must point at a file that exists: otherwise the registry
    // page is the one-line description alone, and nothing about the repository
    // says so.
    let readme = read_key("readme");
    assert!(
        std::path::Path::new(&readme).is_file(),
        "[package] readme = `{readme}`, which is not a file"
    );

    let words: Vec<String> = read_key("keywords")
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|k| k.trim().trim_matches('"').to_string())
        .filter(|k| !k.is_empty())
        .collect();

    assert!(
        !words.is_empty() && words.len() <= 5,
        "crates.io accepts 1 to 5 keywords, there are {} here: {words:?}",
        words.len()
    );
    for k in &words {
        assert!(
            k.len() <= 20,
            "keyword `{k}` is {} characters long: crates.io accepts at most 20",
            k.len()
        );
        assert!(
            k.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
            "keyword `{k}` must start with an alphanumeric character"
        );
        assert!(
            k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "keyword `{k}` contains a character crates.io does not accept"
        );
    }

    let categories = read_key("categories");
    assert!(
        categories.contains('"'),
        "[package] categories declares no category: {categories}"
    );

    // the command the README has people paste names the crate REALLY published.
    // the filename guard (A-V3-17) applied to the fourth package: two sources
    // that must coincide, and a symptom — an install that finds nothing — that
    // never reaches us.
    let name = read_key("name");
    let readme_text = std::fs::read_to_string("README.md").expect("reading README.md");
    assert!(
        readme_text.contains(&format!("cargo install {name}")),
        "README: the command must be `cargo install {name}`, the crate release.yml publishes"
    );
}

/// every release job declares which of the two events it belongs to.
///
/// the workflow hangs on **two** events with different meanings: the tag push
/// builds the artifacts (reversible — delete the draft and retag), the release
/// publication sends the crate to the registry (irreversible — only a yank).
/// without an explicit condition, GitHub runs **every** job on **every** event.
///
/// the test looks at the jobs one by one instead of searching the file for the
/// string: precisely the defect the audit calls "a check that finds the right
/// string in the wrong place". the next job added without one would otherwise
/// surface only on release day.
#[test]
fn every_release_job_declares_which_event_it_belongs_to() {
    const WORKFLOW: &str = ".github/workflows/release.yml";
    let text =
        std::fs::read_to_string(WORKFLOW).unwrap_or_else(|_| panic!("{WORKFLOW} is missing"));

    let body = text
        .split_once("\njobs:\n")
        .map(|(_, after)| after)
        .unwrap_or_else(|| panic!("{WORKFLOW}: the `jobs:` section is missing"));

    // a job is a key at two spaces of indentation; its own keys sit at four.
    // anything deeper belongs to the steps and is none of our business.
    let mut job: Option<String> = None;
    let mut seen: Vec<(String, Option<String>)> = Vec::new();
    for line in body.lines() {
        if let Some(name) = line
            .strip_prefix("  ")
            .filter(|r| !r.starts_with([' ', '#', '-']))
            .and_then(|r| r.split_once(':'))
            .map(|(n, _)| n)
        {
            job = Some(name.to_string());
            seen.push((name.to_string(), None));
        } else if let Some(cond) = line.strip_prefix("    if:") {
            if job.is_some() {
                if let Some(last) = seen.last_mut() {
                    last.1 = Some(cond.trim().to_string());
                }
            }
        }
    }

    // the loop below cannot fail on an EMPTY list: were the parsing to stop
    // recognising jobs, it would iterate nothing and stay green while looking
    // at nothing. so the jobs are demanded by NAME, not by count: a number gets
    // updated absent-mindedly when one is removed, a missing name says which.
    let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
    for expected in ["upload-assets", "deb", "rpm", "crates-io"] {
        assert!(
            names.contains(&expected),
            "{WORKFLOW}: the job `{expected}` is missing; found: {names:?}"
        );
    }

    for (name, cond) in &seen {
        let cond = cond.as_deref().unwrap_or_else(|| {
            panic!(
                "{WORKFLOW}: job `{name}` declares no `if`, so it runs on BOTH events: on \
                 `release: published` the builds would start again"
            )
        });
        if name == "crates-io" {
            assert!(
                cond.contains("github.event_name == 'release'"),
                "{WORKFLOW}: `{name}` must run on `release: published`, not on the tag — there is \
                 no going back on crates.io: {cond}"
            );
            assert!(
                cond.contains("prerelease == false"),
                "{WORKFLOW}: `{name}` must exclude prereleases: crates.io has no draft concept, and \
                 a beta there is indistinguishable from a stable: {cond}"
            );
        } else {
            assert!(
                cond.contains("github.event_name == 'push'"),
                "{WORKFLOW}: `{name}` builds artifacts and must run only on the tag push: \
                 {cond}"
            );
        }
    }
}
