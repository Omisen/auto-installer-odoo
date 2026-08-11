//! Coerenza della configurazione usata dalla CI di integrazione (R5).
//!
//! `configs/ci.env`, `scripts/ci/integration-test.sh` e il parser `.env` devono
//! dire la stessa cosa. Se divergono, il sintomo arriva dopo **quaranta minuti**
//! di job — o peggio, non arriva affatto: una chiave scritta male non è un
//! errore, è un warning, e l'installer prosegue con i default. Il test di
//! integrazione andrebbe a verificare un database `odoo` che nessuno ha creato,
//! e passerebbe o fallirebbe per il motivo sbagliato.
//!
//! Questi test girano nella CI veloce (`test.yml`), su mock, in millisecondi.

use std::path::Path;

use invok::config::{self, ResolvedConfig};

const CI_ENV: &str = "configs/ci.env";
const CI_NGINX_ENV: &str = "configs/ci-nginx.env";
const CI_SCRIPT: &str = "scripts/ci/integration-test.sh";

/// Le chiavi che `config::parse_env_file` riconosce.
///
/// Duplicate qui di proposito, come **guardia**: la fonte di verità resta il
/// `match` in `config.rs`. Se qualcuno aggiunge una chiave a `ci.env` che il
/// parser ignorerebbe in silenzio, questo elenco la fa emergere subito.
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
    // Nome storico, ancora riconosciuto: vive nei `.env` dei clienti (A-V3-6).
    "NGINX_ENABLE_SSL",
];

fn resolve_ci_env() -> ResolvedConfig {
    let raw = config::parse_env_file(Path::new(CI_ENV)).expect("configs/ci.env deve esistere");
    let empty = config::RawConfig::default();
    // `interactive = false`: è come gira in CI, senza TTY. Il flag conta —
    // in non-interattivo la password debole 'admin' è un hard-stop.
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

/// B-V3-7: la config con nginx differisce da quella base **solo** per nginx.
///
/// Se divergessero anche su altro, un fallimento che compare solo nel job nginx
/// non sarebbe più attribuibile a nginx — e il valore di avere due job
/// identici-tranne-uno starebbe tutto lì.
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

/// Il flag che apre la 443 resta **fuori** dalla config di CI, e non per caso:
/// su un runner `ufw` è installato ma inattivo, quindi lo step uscirebbe subito
/// senza aggiungere copertura — e il flag comunque non tocca il vhost (A-V3-6).
#[test]
fn the_nginx_ci_config_does_not_ask_for_the_https_port() {
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");
    assert_eq!(con_nginx.open_https_port, None);
}

#[test]
fn ci_env_resolves_to_what_the_integration_script_expects() {
    let cfg = resolve_ci_env();

    // Il nome del database è il perno del test: NON deve essere il default.
    // Se il rollback ricavasse i nomi dai default invece che dalla config
    // persistita (regressione A-R4-1), cercherebbe 'odoo' e lascerebbe
    // 'citest' sul sistema — e il test di pulizia lo vedrebbe.
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
    // In non-interattivo `resolve` rifiuta la password 'admin'
    // (InsecureAdminNonInteractive): con quel valore la CI non partirebbe
    // nemmeno. `resolve_ci_env` lo prova già col suo `expect`; qui si dice
    // perché, così chi tocca il file sa cosa sta toccando.
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
    // I default dello script devono combaciare coi valori del .env: lo script
    // verifica *per nome* il database, l'utente e la porta, e un disallineamento
    // renderebbe le asserzioni vacue (cercherebbero artefatti che nessuno ha
    // creato) invece di farle fallire.
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

    // `bash -n` non esegue nulla: analizza soltanto la sintassi. Uno script
    // rotto verrebbe scoperto altrimenti solo a metà di un job da 40 minuti,
    // dopo aver già installato mezzo sistema.
    let status = std::process::Command::new("bash")
        .args(["-n", CI_SCRIPT])
        .status()
        .expect("bash deve essere disponibile");
    assert!(status.success(), "sintassi non valida in {CI_SCRIPT}");
}

// --- A5.3: le costanti "ultima release provata" seguono la CI vera ----------

const WORKFLOW: &str = ".github/workflows/integration.yml";

/// `NEWEST_TESTED_*` dicono all'utente su cosa
/// l'installer viene provato davvero. Se divergessero dalla matrice della CI,
/// l'avviso mentirebbe in una delle due direzioni: tacerebbe su una release non
/// provata, o allarmerebbe su una che proviamo.
///
/// La fonte di verità resta il workflow; queste costanti la inseguono, e questo
/// test lo rende obbligatorio invece che auspicabile.
#[test]
fn the_newest_tested_releases_match_the_ci_matrix() {
    use invok::checks::{NEWEST_TESTED_DEBIAN, NEWEST_TESTED_FEDORA, NEWEST_TESTED_UBUNTU};

    let wf = std::fs::read_to_string(WORKFLOW).expect("il workflow di integrazione deve esistere");

    // Ubuntu: `os: [ubuntu-22.04, ubuntu-24.04]` più i `runs-on:` dei job
    // singoli, che non passano dalla matrice.
    let ubuntu_max = versions_in(&wf, "ubuntu-")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Ubuntu");
    assert_eq!(
        ubuntu_max, NEWEST_TESTED_UBUNTU,
        "la CI gira su Ubuntu {ubuntu_max:?} ma la costante dice {NEWEST_TESTED_UBUNTU:?}: \
         l'avviso su release non testate direbbe il falso"
    );

    // Debian: `image: ["debian:12", "debian:11"]`.
    let debian_max = versions_in(&wf, "debian:")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Debian");
    assert_eq!(
        debian_max, NEWEST_TESTED_DEBIAN,
        "la CI gira su Debian {debian_max:?} ma la costante dice {NEWEST_TESTED_DEBIAN:?}"
    );

    // Fedora, e qui la matrice dice DUE cose diverse.
    //
    // Ci sono voci bloccanti (un rosso ferma tutto) e una SONDA su una release
    // mai supportata, tollerata in rosso perché un rosso che ci si aspetta di
    // vedere insegna a ignorare i rossi. La costante deve seguire le sole voci
    // bloccanti: `is_newer_than_tested` promette «release su cui l'installer
    // viene provato», e una release il cui fallimento non ferma nessuno non è
    // provata — è osservata. Contarla direbbe il falso proprio nel senso che
    // rende l'avviso utile.
    let fedora_bloccanti = versions_in(&senza_sonde(&wf), "fedora:");
    let fedora_max = fedora_bloccanti
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Fedora bloccante");
    assert_eq!(
        fedora_max, NEWEST_TESTED_FEDORA,
        "la CI gira su Fedora {fedora_max:?} ma la costante dice {NEWEST_TESTED_FEDORA:?}"
    );

    // E il marcatore non dev'essere un modo per zittire la guardia: una sonda
    // ha senso solo su una release PIÙ RECENTE di quelle provate davvero.
    // Marcare una voce bloccante come sonda la farebbe sparire dal confronto
    // qui sopra senza che nulla lo dica — la stessa forma del difetto che
    // questo test esiste per impedire, un livello più su.
    for sonda in versions_in(&sole_sonde(&wf), "fedora:") {
        assert!(
            sonda > NEWEST_TESTED_FEDORA,
            "la sonda su Fedora {sonda:?} non è più recente di {NEWEST_TESTED_FEDORA:?}: \
             o è una voce bloccante marcata per sbaglio come sonda, o la costante è rimasta \
             indietro rispetto a una release ormai provata davvero"
        );
    }
}

/// Il workflow senza le righe marcate come sonda non bloccante.
///
/// Testuale e non strutturato, come tutto il resto di questa guardia: il test
/// legge il file com'è, senza portarsi dietro un parser YAML per una domanda
/// che si risolve guardando una riga.
fn senza_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| !riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Solo le righe marcate come sonda.
fn sole_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Il commento che marca una voce di matrice come sonda tollerata in rosso.
/// Deve combaciare con `.github/workflows/integration.yml`.
const MARCATORE_SONDA: &str = "sonda-non-bloccante";

/// Tutte le versioni che seguono `prefisso` nel testo, come `(major, minor)`.
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

// --- M5: le due confezioni devono contenere la stessa cosa ------------------

/// `.deb` e `.rpm` depositano **lo stesso binario nello stesso posto**.
///
/// I due strumenti (`cargo deb`, `cargo generate-rpm`) non si parlano e leggono
/// blocchi di metadati diversi: la lista degli asset è scritta due volte, in due
/// sintassi diverse, nello stesso file. Due liste che devono coincidere e
/// nessuno che lo verifichi è il modo in cui si finisce per pubblicare un `.rpm`
/// che non contiene il binario — e nessuno se ne accorge finché un utente non
/// prova a installarlo.
///
/// Non si verifica il *contenuto* dei pacchetti (servirebbe generarli, e i due
/// strumenti non sono installati qui): si verifica che le due dichiarazioni
/// promettano la stessa cosa.
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
        // cargo-deb: ["sorgente", "destinazione/", "mode"]
        ("deb", &deb, "usr/bin/"),
        // cargo-generate-rpm: { source, dest, mode }
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

    // La promessa che vale per tutte e due: impacchettano il TOOL, non Odoo.
    // Nessun servizio, nessuna dipendenza su postgres/nginx — quelle le crea
    // l'installer a runtime, ed è ciò che rende il pacchetto innocuo da
    // installare.
    //
    // Si guardano le **righe di dichiarazione**, non il testo intero: la
    // descrizione nomina PostgreSQL e nginx a ragione (l'installer li
    // configura), e un controllo che cercasse quelle parole ovunque
    // fallirebbe su una frase corretta. Prima versione di questo test: proprio
    // così.
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

/// I comandi di installazione del README puntano alla versione **di questo
/// pacchetto**, non a una passata.
///
/// Il README non usa più variabili di shell (`VER=…`): i comandi sono stringhe
/// intere, copiabili senza leggerle. È la forma giusta per chi installa — e
/// moltiplica per **undici** i punti in cui una versione può restare indietro.
/// Due fonti che devono coincidere e nessuno che lo verifichi è il modo in cui
/// si finisce per far scaricare ai clienti la release precedente, in silenzio:
/// il comando funziona, il file esiste, e nessuno se ne accorge.
///
/// Si verifica la corrispondenza con `Cargo.toml`, che è la versione che il
/// workflow di release taggherà — quindi il README è aggiornato quando lo è il
/// manifesto, e non «quando qualcuno si ricorda».
///
/// **Il numero della release non era però l'unico modo di sbagliare il nome del
/// file** (A-V3-17): la v2.3.0 aveva la versione giusta ovunque e il `.deb`
/// restava irraggiungibile, perché al nome mancava la revisione `-1` che
/// `cargo-deb` aggiunge. Da qui i nomi composti da [`package_file_name`] invece
/// che scritti a mano qui sotto.
#[test]
fn the_readme_download_commands_point_at_this_version() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let versione = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml deve dichiarare una versione");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // Ogni URL di download nomina QUESTA versione.
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

    // E i nomi dei file, che si compongono con la REVISIONE dichiarata nel
    // manifesto — non con una scritta a mano qui (A-V3-17).
    for atteso in [package_file_name("deb"), package_file_name("rpm")] {
        assert!(
            readme.contains(&atteso),
            "il README non nomina `{atteso}`: il comando di installazione scaricherebbe un file \
             che quella release non contiene"
        );
    }
}

/// La revisione del pacchetto è **dichiarata** in `Cargo.toml`, non ereditata.
///
/// **A-V3-17.** Il `.deb` della v2.3.0 si chiama `invok_2.3.0-1_amd64.deb`
/// e il README ne nominava uno senza `-1`: chi seguiva il comando di
/// installazione otteneva un 404. Il `-1` c'era perché `cargo-deb` aggiunge una
/// revisione di default — cioè il nome dell'artefatto, che il README promette
/// per intero, veniva deciso **fuori dal repository**, da un default che può
/// cambiare fra versioni dello strumento.
///
/// Scriverla qui non è pignoleria: è ciò che rende la guardia qui sopra capace
/// di dire di no. Finché il nome atteso era una stringa scritta a mano nel test,
/// il test e il README ripetevano la stessa congettura e nessuno dei due leggeva
/// lo strumento — un controllo che nello scenario per cui esiste non può fallire,
/// la firma ricorrente di questo progetto.
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

/// Il valore di `chiave` dentro `sezione` di `Cargo.toml`, se c'è.
///
/// Parsing per sezione e non per riga: `release` da solo comparirebbe anche in
/// un `[profile.release]`, e leggere la chiave giusta nella sezione sbagliata è
/// il modo in cui una guardia sembra funzionare mentre misura altro.
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

/// Il nome del file che quella confezione produce, composto dal manifesto.
///
/// La **forma** del nome resta scritta qui — è convenzione di `cargo-deb` e di
/// `cargo-generate-rpm`, non un dato che il repository possieda — mentre versione
/// e revisione si leggono. Il che lascia un residuo di congettura, ed è
/// dichiarato: a verificarlo contro il file **davvero prodotto** è
/// `release.yml`, che il pacchetto ce l'ha in mano prima di pubblicarlo. Questa
/// è la guardia veloce; quella è l'ultima parola.
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

/// La versione che il binario dichiara è quella di `Cargo.toml`.
///
/// `INSTALLER_VERSION` viene da `env!("CARGO_PKG_VERSION")`, quindi oggi non può
/// divergere — e questo test esiste proprio per il giorno in cui qualcuno,
/// volendo «renderla configurabile», la trasformasse in una costante scritta a
/// mano. È la stessa guardia del README, applicata al terzo consumatore della
/// versione: flag, log e manifesto devono dire tutti lo stesso numero (A-V3-16).
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

// --- Non affiliazione: la stessa promessa in tre confezioni -----------------

/// Il disclaimer di non affiliazione a Odoo S.A. c'è **ovunque il pacchetto si
/// presenti**: README, `.deb` e `.rpm`.
///
/// Non è una formalità: il perno della questione marchio è che nessuno confonda
/// questo strumento con un prodotto di Odoo S.A., e chi installa da `apt`/`dnf`
/// il README non lo apre — legge `apt show` / `dnf info`. Un disclaimer che vive
/// solo nel README protegge esattamente il lettore che non ne aveva bisogno.
///
/// La forma delle tre frasi è diversa per necessità (il `.rpm` non ha un campo
/// di descrizione lunga: `cargo-generate-rpm` espone solo `summary`), quindi non
/// si confrontano i testi fra loro — si pretende che ciascuno **nomini Odoo S.A.
/// e neghi l'affiliazione**. È il minimo che rende la promessa verificabile
/// senza congelare la formulazione.
#[test]
fn every_package_face_disclaims_affiliation_with_odoo() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // Il blocco di metadati, isolato: cercare la frase nel manifesto intero
    // passerebbe anche se il disclaimer stesse solo in un commento — e un
    // commento non finisce in nessun pacchetto.
    let blocco = |intestazione: &str| -> String {
        let inizio = manifest
            .find(intestazione)
            .unwrap_or_else(|| panic!("manca il blocco {intestazione}"));
        let resto = &manifest[inizio + intestazione.len()..];
        let fine = resto.find("\n[").unwrap_or(resto.len());
        // Via i commenti: il disclaimer deve stare in un VALORE.
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
            "{dove}: il disclaimer deve nominare il titolare del marchio"
        );
        assert!(
            testo.contains("non affiliato a Odoo S.A.")
                || testo.contains("non è affiliato a Odoo S.A."),
            "{dove}: manca la negazione esplicita dell'affiliazione"
        );
    }
}

// --- Le due confezioni installano lo STESSO alias -------------------------

/// `.deb` e `.rpm` creano lo stesso alias `vok`, con le stesse due cautele.
///
/// Gli script sono quattro file distinti perché le **guardie** devono divergere:
/// `postinst` riceve `configure`, `%post` non riceve niente di utile; `postrm`
/// riceve `remove`/`upgrade`, `%postun` riceve il numero di installazioni
/// rimaste (`0` = rimozione vera). Quella parte è convenzione del gestore di
/// pacchetti e non si può unificare.
///
/// Ciò che invece **non** deve divergere è l'AZIONE, ed è quella che si verifica
/// qui: stesso link, stesso bersaglio, e le due cautele — non sovrascrivere un
/// file di qualcun altro, non rimuovere un link ripuntato altrove. Senza questo
/// test si avrebbero due copie della stessa logica in due formati diversi, che è
/// il modo in cui in questo progetto una delle due resta indietro.
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
        // La cautela: un `/usr/bin/vok` che non è un symlink appartiene a
        // qualcun altro e non si sovrascrive.
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
        // Si rimuove solo il NOSTRO link: se punta altrove, non è nostro.
        assert!(
            script.contains(r#"[ "$(readlink /usr/bin/vok)" = "invok" ]"#),
            "{confezione}: deve rimuovere solo un link che punta ancora a invok"
        );
    }
    // E solo alla rimozione vera, mai durante un aggiornamento — scritto nelle
    // due convenzioni diverse, che è esattamente perché i file sono due.
    assert!(
        rimuovono[0].1.contains("remove | purge") || rimuovono[0].1.contains("remove|purge"),
        "deb: la rimozione dell'alias non deve avvenire su `upgrade`"
    );
    assert!(
        rimuovono[1].1.contains(r#"[ "$1" = "0" ]"#),
        "rpm: la rimozione dell'alias deve avvenire solo con $1 = 0 (disinstallazione vera)"
    );

    // I due percorsi dichiarati in Cargo.toml devono ESISTERE: `cargo-generate-rpm`
    // accetta indifferentemente uno script inline o un percorso, e distingue in
    // base all'esistenza del file. Un percorso sbagliato non dà errore — finisce
    // nel pacchetto come comando letterale, e si scopre sulla macchina di un
    // cliente. È un controllo che qui può dire di no, e là no.
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

// --- crates.io: la quarta confezione, e l'unica irreversibile --------------

/// I metadati che crates.io pretende sono presenti e nella norma.
///
/// Questi vincoli — al massimo 5 keyword, ognuna ≤ 20 caratteri e con il primo
/// carattere alfanumerico, categorie da una lista chiusa — li fa rispettare il
/// **registro**, non `cargo build`. Una violazione quindi non si vede né
/// compilando né testando: si vede quando `cargo publish` la rifiuta, cioè dopo
/// che il tag è stato spinto e la release pubblicata. Il momento peggiore, per
/// il canale che è l'unico a non avere un annulla.
///
/// La lista delle categorie NON si verifica qui: è chiusa e vive su crates.io,
/// e ricopiarla vorrebbe dire tenere allineata una quarta copia di un dato che
/// non possediamo. A dire di no su quella è `cargo publish --dry-run` in
/// `release.yml`, che interroga il registro vero. Qui si verifica ciò che il
/// repository sa di sé.
#[test]
fn the_crate_metadata_is_publishable() {
    let leggi = |chiave: &str| {
        manifest_value("[package]", chiave)
            .unwrap_or_else(|| panic!("[package] deve dichiarare `{chiave}` per pubblicare"))
    };

    // `readme` deve puntare a un file che esiste: se non esiste, la scheda su
    // crates.io resta la sola `description` di una riga e nessuno se ne accorge
    // guardando il repository.
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

    // Il comando che il README fa incollare nomina il crate DAVVERO pubblicato.
    // È la stessa guardia dei nomi di file .deb/.rpm (A-V3-17) applicata alla
    // quarta confezione: due fonti che devono coincidere, e un sintomo — un
    // `cargo install` che non trova nulla — che non arriva mai a noi.
    let nome = leggi("name");
    let readme_testo = std::fs::read_to_string("README.md").expect("leggo README.md");
    assert!(
        readme_testo.contains(&format!("cargo install {nome}")),
        "README: il comando deve essere `cargo install {nome}`, il crate che release.yml pubblica"
    );
}

/// Ogni job di `release.yml` dichiara a quale dei due eventi appartiene.
///
/// Il workflow è agganciato a **due** eventi con due significati diversi: il
/// push del tag costruisce gli artefatti (reversibile: si cancella la bozza e si
/// ritagga), la pubblicazione della release manda il crate su crates.io
/// (irreversibile: si può solo `yank`). Senza un `if` esplicito, GitHub esegue
/// **ogni** job a **ogni** evento — quindi alla pubblicazione ripartirebbero i
/// build e `create-gh-release-action` proverebbe a creare una release che esiste
/// già.
///
/// Il test guarda i job uno per uno invece di cercare la stringa da qualche
/// parte nel file: è precisamente il difetto che l'audit chiama «un controllo
/// che trova la stringa giusta nel posto sbagliato», e qui basterebbe che il
/// prossimo job aggiunto ne fosse sprovvisto perché il sintomo arrivi solo il
/// giorno della release.
#[test]
fn every_release_job_declares_which_event_it_belongs_to() {
    const WORKFLOW: &str = ".github/workflows/release.yml";
    let testo = std::fs::read_to_string(WORKFLOW).unwrap_or_else(|_| panic!("manca {WORKFLOW}"));

    let corpo = testo
        .split_once("\njobs:\n")
        .map(|(_, dopo)| dopo)
        .unwrap_or_else(|| panic!("{WORKFLOW}: manca la sezione `jobs:`"));

    // Un job è una chiave a DUE spazi d'indentazione; le sue chiavi (`if`,
    // `needs`, `runs-on`…) stanno a quattro. Tutto ciò che sta più a fondo
    // appartiene agli step e non ci riguarda.
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

    assert!(
        visti.len() >= 5,
        "{WORKFLOW}: attesi almeno 5 job, trovati {}: {:?}",
        visti.len(),
        visti.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

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
