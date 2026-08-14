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
    let raw = config::parse_env_file(Path::new(CI_ENV)).expect("configs/ci.env deve esistere");
    let empty = config::RawConfig::default();
    // non-interactive, the way CI runs it. the flag matters: there the weak
    // default password is a hard stop.
    ResolvedConfig::resolve(&empty, &raw, &empty, false)
        .expect("configs/ci.env deve risolvere senza intervento interattivo")
}

#[test]
fn every_key_in_ci_env_is_understood_by_the_parser() {
    for file in [CI_ENV, CI_NGINX_ENV] {
        assert_keys_are_known(file);
    }
}

fn assert_keys_are_known(file: &str) {
    let content = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("{file} deve esistere"));
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
            .unwrap_or_else(|| panic!("riga {} senza '=': {line}", lineno + 1));
        assert!(
            KNOWN_KEYS.contains(&key),
            "{file} riga {}: la chiave '{key}' non è riconosciuta dal parser .env. \
             Verrebbe ignorata con un warning e l'installer userebbe il default, \
             falsando il test di integrazione.",
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
        "la config base resta senza nginx"
    );
    assert_eq!(
        con_nginx.with_nginx,
        Some(true),
        "è l'unica ragione per cui questo file esiste"
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
        "il db_name della CI deve differire dal default, altrimenti il test non \
         distingue 'il rollback ha usato la config persistita' da 'ha indovinato'"
    );

    assert_eq!(cfg.version, "18.0");
    assert_eq!(cfg.version_short, "18");
    assert_eq!(cfg.odoo_user, "odoo");
    assert_eq!(cfg.db_user, "odoo");
    assert_eq!(cfg.port, 8069);
    assert!(!cfg.with_nginx, "la sonda di CI non configura Nginx");
    assert!(
        cfg.odoo_logfile.is_none(),
        "ODOO_LOGFILE vuoto = log su journal: nessuna log dir da verificare"
    );
    assert!(
        cfg.db_password.is_empty(),
        "password vuota = autenticazione peer, il percorso che vogliamo esercitare"
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
        "una password 'admin' fa fallire l'installazione non interattiva prima \
         di qualsiasi step"
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
            "lo script di integrazione deve avere `${{{var}:-{expected}}}` per \
             combaciare con configs/ci.env (atteso il frammento `{needle}`)"
        );
    }
}

#[test]
fn the_integration_script_is_executable_and_syntactically_valid() {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(CI_SCRIPT).expect("lo script deve esistere");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "lo script di integrazione deve avere il bit di esecuzione"
    );

    // a syntax-only check runs nothing. a broken script would otherwise surface
    // halfway through a forty-minute job, with half a system already installed.
    let status = std::process::Command::new("bash")
        .args(["-n", CI_SCRIPT])
        .status()
        .expect("bash deve essere disponibile");
    assert!(status.success(), "sintassi non valida in {CI_SCRIPT}");
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

    let wf = std::fs::read_to_string(WORKFLOW).expect("il workflow di integrazione deve esistere");

    // the matrix entries plus the individual jobs' runners, which do not go
    // through it.
    let ubuntu_max = versions_in(&wf, "ubuntu-")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Ubuntu");
    assert_eq!(
        ubuntu_max, NEWEST_TESTED_UBUNTU,
        "la CI gira su Ubuntu {ubuntu_max:?} ma la costante dice {NEWEST_TESTED_UBUNTU:?}: \
         l'avviso su release non testate direbbe il falso"
    );

    // the container images.
    let debian_max = versions_in(&wf, "debian:")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Debian");
    assert_eq!(
        debian_max, NEWEST_TESTED_DEBIAN,
        "la CI gira su Debian {debian_max:?} ma la costante dice {NEWEST_TESTED_DEBIAN:?}"
    );

    // here the matrix says TWO different things: blocking entries, where a red
    // stops everything, and a PROBE on a never-supported release, tolerated red
    // because an expected red teaches people to ignore reds.
    //
    // the constant must follow the blocking entries only: the warning promises
    // "releases the installer is tested on", and a release whose failure stops
    // nobody is observed, not tested.
    let fedora_bloccanti = versions_in(&senza_sonde(&wf), "fedora:");
    let fedora_max = fedora_bloccanti
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Fedora bloccante");
    assert_eq!(
        fedora_max, NEWEST_TESTED_FEDORA,
        "la CI gira su Fedora {fedora_max:?} ma la costante dice {NEWEST_TESTED_FEDORA:?}"
    );

    // and the marker must not become a way to silence the guard: a probe only
    // makes sense on a release NEWER than the tested ones. marking a blocking
    // entry as a probe would remove it from the comparison above with nothing
    // to say so — the same defect this test exists to prevent, one level up.
    for sonda in versions_in(&sole_sonde(&wf), "fedora:") {
        assert!(
            sonda > NEWEST_TESTED_FEDORA,
            "la sonda su Fedora {sonda:?} non è più recente di {NEWEST_TESTED_FEDORA:?}: \
             o è una voce bloccante marcata per sbaglio come sonda, o la costante è rimasta \
             indietro rispetto a una release ormai provata davvero"
        );
    }
}

/// the workflow without the lines marked as a non-blocking probe.
///
/// textual, like the rest of this guard: the file is read as it is, without a
/// YAML parser for a question one line answers.
fn senza_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| !riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// only the lines marked as a probe.
fn sole_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// the comment marking a matrix entry as a probe tolerated in red. must match
/// the workflow.
const MARCATORE_SONDA: &str = "sonda-non-bloccante";

/// every version following `prefix` in the text, as `(major, minor)`.
fn versions_in(text: &str, prefisso: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (_, resto) in text
        .match_indices(prefisso)
        .map(|(i, m)| (i, &text[i + m.len()..]))
    {
        let numero: String = resto
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if numero.is_empty() {
            continue;
        }
        let mut parti = numero.split('.');
        let major = parti.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parti.next().and_then(|s| s.parse().ok()).unwrap_or(0);
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
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");

    let blocco = |intestazione: &str| -> String {
        let inizio = manifest
            .find(intestazione)
            .unwrap_or_else(|| panic!("manca il blocco {intestazione}"));
        let resto = &manifest[inizio + intestazione.len()..];
        let fine = resto.find("\n[").unwrap_or(resto.len());
        resto[..fine].to_string()
    };

    let deb = blocco("[package.metadata.deb]");
    let rpm = blocco("[package.metadata.generate-rpm]");

    for (nome, testo, destinazione) in [
        // one tool: ["source", "destination/", "mode"]
        ("deb", &deb, "usr/bin/"),
        // the other: { source, dest, mode }
        ("rpm", &rpm, "/usr/bin/invok"),
    ] {
        assert!(
            testo.contains("target/release/invok"),
            "{nome}: deve impacchettare il binario compilato, non altro"
        );
        assert!(
            testo.contains(destinazione),
            "{nome}: il binario deve finire in /usr/bin, o il comando non è nel PATH"
        );
        assert!(
            testo.contains("755"),
            "{nome}: un binario senza bit di esecuzione non è un binario"
        );
        assert!(
            testo.contains("README"),
            "{nome}: la doc accompagna il pacchetto in entrambe le confezioni"
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
    let dichiarazioni = |testo: &str, chiavi: &[&str]| -> String {
        testo
            .lines()
            .filter(|riga| {
                let r = riga.trim_start();
                chiavi.iter().any(|k| r.starts_with(k))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    for (nome, testo) in [("deb", &deb), ("rpm", &rpm)] {
        assert!(
            !testo.contains("systemd-units"),
            "{nome}: il pacchetto non installa servizi"
        );
        let deps = dichiarazioni(testo, &["depends", "requires", "recommends", "suggests"]);
        assert!(
            !deps.contains("postgresql") && !deps.contains("nginx") && !deps.contains("python"),
            "{nome}: nessuna dipendenza su ciò che l'installer gestisce a runtime, \
             trovato: {deps}"
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
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let versione = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml deve dichiarare una versione");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // every download URL names THIS version.
    let mut url = 0;
    for pezzo in readme.split("releases/download/v").skip(1) {
        url += 1;
        let trovata = pezzo.split('/').next().unwrap_or("");
        assert_eq!(
            trovata, versione,
            "il README scarica dalla release v{trovata} mentre il pacchetto è {versione}"
        );
    }
    assert!(
        url >= 6,
        "attesi almeno sei link di download (tar.gz, .deb, .rpm, con i rispettivi .sha256), \
         trovati {url}: se la sezione è cambiata, questa guardia va aggiornata insieme"
    );

    // and the filenames, composed with the REVISION declared in the manifest —
    // not one written by hand here (A-V3-17).
    for atteso in [package_file_name("deb"), package_file_name("rpm")] {
        assert!(
            readme.contains(&atteso),
            "il README non nomina `{atteso}`: il comando di installazione scaricherebbe un file \
             che quella release non contiene"
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
    for (sezione, chiave) in [
        ("[package.metadata.deb]", "revision"),
        ("[package.metadata.generate-rpm]", "release"),
    ] {
        assert!(
            manifest_value(sezione, chiave).is_some(),
            "{sezione} deve dichiarare `{chiave}`: senza, il nome del file pubblicato lo decide \
             il default dello strumento, e il README promette un nome che nessuno controlla"
        );
    }
}

/// the value of `key` inside `section` of the manifest, if present.
///
/// parsed by section and not by line: a bare key name also appears in a profile
/// block, and reading the right key in the wrong section is how a guard looks
/// like it works while measuring something else.
fn manifest_value(sezione: &str, chiave: &str) -> Option<String> {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let inizio = manifest.find(sezione)? + sezione.len();
    let resto = &manifest[inizio..];
    let blocco = &resto[..resto.find("\n[").unwrap_or(resto.len())];
    blocco.lines().find_map(|riga| {
        riga.trim()
            .strip_prefix(chiave)?
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
    let versione = manifest_value("[package]", "version").expect("versione nel manifesto");
    match confezione {
        "deb" => {
            let rev = manifest_value("[package.metadata.deb]", "revision")
                .expect("revisione del .deb nel manifesto");
            format!("invok_{versione}-{rev}_amd64.deb")
        }
        _ => {
            let rel = manifest_value("[package.metadata.generate-rpm]", "release")
                .expect("release del .rpm nel manifesto");
            format!("invok-{versione}-{rel}.x86_64.rpm")
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
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let dichiarata = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml deve dichiarare una versione");

    assert_eq!(
        invok::INSTALLER_VERSION,
        dichiarata,
        "il binario dice di essere {} ma il pacchetto è {dichiarata}",
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
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // The metadata block, isolated: searching the whole manifest would pass
    // even if the disclaimer lived only in a comment — and a comment ends up in
    // no package.
    let blocco = |intestazione: &str| -> String {
        let inizio = manifest
            .find(intestazione)
            .unwrap_or_else(|| panic!("manca il blocco {intestazione}"));
        let resto = &manifest[inizio + intestazione.len()..];
        let fine = resto.find("\n[").unwrap_or(resto.len());
        // Comments out: the disclaimer must live in a VALUE.
        resto[..fine]
            .lines()
            .filter(|r| !r.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    };

    for (dove, testo) in [
        ("README.md", readme.as_str()),
        (
            ".deb (extended-description)",
            &blocco("[package.metadata.deb]"),
        ),
        (".rpm (summary)", &blocco("[package.metadata.generate-rpm]")),
    ] {
        assert!(
            testo.contains("Odoo S.A."),
            "{dove}: the disclaimer must name the trademark holder"
        );
        // Case-insensitive: the sentence opens a paragraph in one place and
        // sits mid-sentence in another, and freezing the capital letter would
        // fail on a correct text.
        assert!(
            testo
                .to_lowercase()
                .contains("not affiliated with odoo s.a."),
            "{dove}: the explicit denial of affiliation is missing"
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
    let leggi = |p: &str| std::fs::read_to_string(p).unwrap_or_else(|_| panic!("manca {p}"));

    let installano = [
        ("deb", leggi("debian/postinst")),
        ("rpm", leggi("rpm/post.sh")),
    ];
    for (confezione, script) in &installano {
        assert!(
            script.contains("ln -sfn invok /usr/bin/vok"),
            "{confezione}: l'alias deve essere un symlink RELATIVO a `invok`"
        );
        // the caution: a target that is not a symlink belongs to somebody else
        // and is not overwritten.
        assert!(
            script.contains("[ ! -L /usr/bin/vok ]"),
            "{confezione}: non deve sovrascrivere un /usr/bin/vok che non sia un collegamento"
        );
    }

    let rimuovono = [
        ("deb", leggi("debian/postrm")),
        ("rpm", leggi("rpm/postun.sh")),
    ];
    for (confezione, script) in &rimuovono {
        // only OUR link is removed: pointing elsewhere means it is not ours.
        assert!(
            script.contains(r#"[ "$(readlink /usr/bin/vok)" = "invok" ]"#),
            "{confezione}: deve rimuovere solo un link che punta ancora a invok"
        );
    }
    // and only on a real removal, never during an upgrade — written in the two
    // different conventions, which is exactly why there are two files.
    assert!(
        rimuovono[0].1.contains("remove | purge") || rimuovono[0].1.contains("remove|purge"),
        "deb: la rimozione dell'alias non deve avvenire su `upgrade`"
    );
    assert!(
        rimuovono[1].1.contains(r#"[ "$1" = "0" ]"#),
        "rpm: la rimozione dell'alias deve avvenire solo con $1 = 0 (disinstallazione vera)"
    );

    // the declared paths must EXIST: the tool accepts either an inline script
    // or a path and tells them apart by whether the file is there. a wrong path
    // is no error — it lands in the package as a literal command, and shows up
    // on a customer's machine.
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    for campo in ["post_install_script", "post_uninstall_script"] {
        let percorso = manifest
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{campo} = ")))
            .map(|v| v.trim().trim_matches('"'))
            .unwrap_or_else(|| panic!("Cargo.toml deve dichiarare {campo}"));
        assert!(
            Path::new(percorso).is_file(),
            "{campo} punta a `{percorso}`, che non è un file: finirebbe nel .rpm \
             come comando letterale invece che come scriptlet"
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
    let leggi = |chiave: &str| {
        manifest_value("[package]", chiave)
            .unwrap_or_else(|| panic!("[package] deve dichiarare `{chiave}` per pubblicare"))
    };

    // the readme key must point at a file that exists: otherwise the registry
    // page is the one-line description alone, and nothing about the repository
    // says so.
    let readme = leggi("readme");
    assert!(
        std::path::Path::new(&readme).is_file(),
        "[package] readme = `{readme}`, che non è un file"
    );

    let parole: Vec<String> = leggi("keywords")
        .trim_matches(|c| c == '[' || c == ']')
        .split(',')
        .map(|k| k.trim().trim_matches('"').to_string())
        .filter(|k| !k.is_empty())
        .collect();

    assert!(
        !parole.is_empty() && parole.len() <= 5,
        "crates.io accetta da 1 a 5 keyword, qui ce ne sono {}: {parole:?}",
        parole.len()
    );
    for k in &parole {
        assert!(
            k.len() <= 20,
            "la keyword `{k}` è di {} caratteri: crates.io ne accetta al massimo 20",
            k.len()
        );
        assert!(
            k.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()),
            "la keyword `{k}` deve iniziare con un carattere alfanumerico"
        );
        assert!(
            k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "la keyword `{k}` contiene un carattere che crates.io non accetta"
        );
    }

    let categorie = leggi("categories");
    assert!(
        categorie.contains('"'),
        "[package] categories non dichiara nessuna categoria: {categorie}"
    );

    // the command the README has people paste names the crate REALLY published.
    // the filename guard (A-V3-17) applied to the fourth package: two sources
    // that must coincide, and a symptom — an install that finds nothing — that
    // never reaches us.
    let nome = leggi("name");
    let readme_testo = std::fs::read_to_string("README.md").expect("leggo README.md");
    assert!(
        readme_testo.contains(&format!("cargo install {nome}")),
        "README: il comando deve essere `cargo install {nome}`, il crate che release.yml pubblica"
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
    let testo = std::fs::read_to_string(WORKFLOW).unwrap_or_else(|_| panic!("manca {WORKFLOW}"));

    let corpo = testo
        .split_once("\njobs:\n")
        .map(|(_, dopo)| dopo)
        .unwrap_or_else(|| panic!("{WORKFLOW}: manca la sezione `jobs:`"));

    // a job is a key at two spaces of indentation; its own keys sit at four.
    // anything deeper belongs to the steps and is none of our business.
    let mut job: Option<String> = None;
    let mut visti: Vec<(String, Option<String>)> = Vec::new();
    for riga in corpo.lines() {
        if let Some(nome) = riga
            .strip_prefix("  ")
            .filter(|r| !r.starts_with([' ', '#', '-']))
            .and_then(|r| r.split_once(':'))
            .map(|(n, _)| n)
        {
            job = Some(nome.to_string());
            visti.push((nome.to_string(), None));
        } else if let Some(cond) = riga.strip_prefix("    if:") {
            if job.is_some() {
                if let Some(ultimo) = visti.last_mut() {
                    ultimo.1 = Some(cond.trim().to_string());
                }
            }
        }
    }

    // the loop below cannot fail on an EMPTY list: were the parsing to stop
    // recognising jobs, it would iterate nothing and stay green while looking
    // at nothing. so the jobs are demanded by NAME, not by count: a number gets
    // updated absent-mindedly when one is removed, a missing name says which.
    let nomi: Vec<&str> = visti.iter().map(|(n, _)| n.as_str()).collect();
    for atteso in ["upload-assets", "deb", "rpm", "crates-io"] {
        assert!(
            nomi.contains(&atteso),
            "{WORKFLOW}: manca il job `{atteso}`; trovati: {nomi:?}"
        );
    }

    for (nome, cond) in &visti {
        let cond = cond.as_deref().unwrap_or_else(|| {
            panic!(
                "{WORKFLOW}: il job `{nome}` non dichiara un `if`, quindi gira a ENTRAMBI gli \
                 eventi: al `release: published` ripartirebbero i build"
            )
        });
        if nome == "crates-io" {
            assert!(
                cond.contains("github.event_name == 'release'"),
                "{WORKFLOW}: `{nome}` deve girare sul `release: published`, non sul tag — su \
                 crates.io non si torna indietro: {cond}"
            );
            assert!(
                cond.contains("prerelease == false"),
                "{WORKFLOW}: `{nome}` deve escludere le prerelease: crates.io non ha il concetto \
                 di bozza, e una beta lì è indistinguibile da una stabile: {cond}"
            );
        } else {
            assert!(
                cond.contains("github.event_name == 'push'"),
                "{WORKFLOW}: `{nome}` costruisce artefatti e deve girare solo sul push del tag: \
                 {cond}"
            );
        }
    }
}
