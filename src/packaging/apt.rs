//! Il backend **apt/dpkg**: comandi e nomi della famiglia Debian.
//!
//! Contiene ciò che fino alla 2.2.0 era sparso fra la sezione «apt / dpkg» del
//! trait `SystemOps` e le costanti di `steps::apt_packages`. Non c'è alcun
//! cambiamento di comportamento: gli stessi comandi, con gli stessi argomenti,
//! nello stesso ordine.

use std::path::Path;

use super::{availability_from, Availability, CatalogEntry, DepId, PackageCatalog, PackageManager};
use crate::error::StepError;
use crate::system_ops::{
    capture_command_with_env, has_installable_candidate, run_command_with_env, total_package_names,
};

/// Esegue `apt-get` con l'ambiente non-interattivo (niente prompt tzdata /
/// needrestart), come il Bash originale.
///
/// `DEBIAN_FRONTEND` e `NEEDRESTART_MODE` sono variabili **Debian-specifiche**:
/// e' il motivo per cui questa funzione vive qui e non fra gli helper generici
/// di `system_ops`.
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

/// Prerequisiti bootstrap: utility comuni a basso rischio.
///
/// Questi quattro nomi sono stabili su tutte le release Debian/Ubuntu
/// supportate, quindi non hanno alternative.
fn bootstrap_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        CatalogEntry::new(DepId::Wget, &["wget"]),
        CatalogEntry::new(DepId::Gettext, &["gettext-base"]),
    ]
}

/// Dipendenze di sistema di Odoo sulla famiglia Debian (lista canonica, da
/// `system.sh`).
///
/// Ogni voce può avere alternative in ordine di preferenza: il primo nome
/// installabile vince. I gruppi con più di un nome sono quelli che cambiano tra
/// release — le divergenze osservate in campo su Debian 11/12 (A5.1).
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
        // Su Ubuntu 24.04 `libfreetype6-dev` è diventato un nome puramente
        // virtuale (`Provides` di `libfreetype-dev`): installabile ma non
        // purgabile. Il nome reale come alternativa fa sì che il delta contenga
        // qualcosa che l'undo possa davvero rimuovere (A5.1-bis).
        CatalogEntry::new(DepId::Freetype, &["libfreetype6-dev", "libfreetype-dev"]),
        CatalogEntry::new(DepId::Xml2, &["libxml2-dev"]),
        CatalogEntry::new(DepId::Zip, &["libzip-dev"]),
        CatalogEntry::new(DepId::Ldap, &["libldap2-dev"]),
        CatalogEntry::new(DepId::Sasl, &["libsasl2-dev"]),
        CatalogEntry::new(DepId::Jpeg, &["libjpeg-dev"]),
        CatalogEntry::new(DepId::Zlib, &["zlib1g-dev"]),
        CatalogEntry::new(DepId::PostgresClient, &["libpq-dev"]),
        CatalogEntry::new(DepId::Xslt, &["libxslt1-dev"]),
        // Rinominato senza il soname: Ubuntu 22.04 ha entrambi, Debian 12 solo
        // il secondo.
        CatalogEntry::new(DepId::Tiff, &["libtiff5-dev", "libtiff-dev"]),
        // Su Ubuntu è un pacchetto di transizione verso `libjpeg-turbo8-dev`; su
        // Debian 12 non esiste e la copertura la dà `libjpeg-dev`, già in lista.
        //
        // Nota (A-MD-1): su una release dove nessuno dei primi due esiste, questa
        // voce risolve a `libjpeg-dev`, **lo stesso nome** di `DepId::Jpeg`. La
        // risoluzione deduplica i nomi risolti prima di comporre il delta: il
        // manifesto è la contabilità di ciò che abbiamo aggiunto, e una
        // contabilità con una riga doppia è una contabilità sbagliata.
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
        // Opzionale: `node-less` è il compilatore degli asset `.less`. Odoo
        // moderno usa SCSS (compilato in-process da libsass) e parte senza
        // `lessc`; il pacchetto è però stato rimosso da alcune release Debian, e
        // una dipendenza da "nice to have" non deve trasformarsi in
        // un'installazione impossibile.
        CatalogEntry::optional(DepId::LessCompiler, &["node-less"]),
    ]
}

/// Pacchetti che installano il server PostgreSQL su Debian/Ubuntu.
pub const POSTGRES_PACKAGES: &[&str] = &["postgresql", "postgresql-contrib"];
/// Il nome con cui si chiede «PostgreSQL è installato?».
pub const POSTGRES_MARKER_PACKAGE: &str = "postgresql";
/// Il pacchetto di nginx.
pub const NGINX_PACKAGE: &str = "nginx";

/// Gli argomenti di `apt-get install`, come funzione **pura**.
///
/// Come per dnf: il codice che esegue apt gira solo su una macchina reale, e
/// `--no-install-recommends` è una protezione del delta (senza, entrano
/// pacchetti che nessuno ha chiesto e che l'undo poi rimuoverebbe). Averla come
/// valore di ritorno la rende verificabile.
pub fn install_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "install".to_string(),
        "-y".to_string(),
        "--no-install-recommends".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// Gli argomenti di `apt-get purge`, come funzione **pura**.
///
/// apt non rimuove le dipendenze orfane se non glielo si chiede, quindi
/// l'invariante di [`PackageManager::remove`] — rimuovere **solo** ciò che è
/// stato chiesto — è soddisfatta senza opzioni aggiuntive. Su dnf non è così, ed
/// è la differenza che il Bivio 2 ha dovuto decidere.
pub fn remove_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec!["purge".to_string(), "-y".to_string()];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// Il gestore di pacchetti della famiglia Debian.
///
/// Senza stato: le due strutture di `RealSystemOps` sono `'static` e i comandi
/// vengono eseguiti a ogni chiamata, come prima.
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
            // Non riusciamo a chiederlo: trattiamolo come indice non
            // interrogabile. Porta a un messaggio prudente ("aggiorna
            // l'indice"), non a un verdetto di assenza.
            Err(_) => false,
        }
    }

    /// Due comandi, in quest'ordine, ed è il **meccanismo** di apt — non la
    /// politica, che sta in `AptPackagesStep::resolve`.
    ///
    /// 1. `apt-cache policy` è la via veloce e copre tutti i casi normali. Un
    ///    `Candidate:` diverso da `(none)` significa che dopo l'installazione
    ///    `dpkg-query` conoscerà questo nome: [`Availability::Real`].
    /// 2. se non c'è candidato reale, resta da distinguere «nome inesistente» da
    ///    «nome puramente virtuale», e a questo risponde solo il risolutore:
    ///    `apt-get install -s` (simulazione, non muta nulla) esce 0 anche per un
    ///    `Provides` con un solo fornitore. È più lenta (~0.4s: fa girare il
    ///    risolutore), per questo si usa come ripiego e non come prima domanda.
    ///
    /// Costo rispetto alla 2.2.0: una simulazione in più per ogni **alternativa
    /// assente** di un gruppo (tre gruppi in tutta la lista, ~1s su
    /// un'installazione che dura minuti). In cambio il chiamante non deve più
    /// sapere in che ordine porre due domande e perché.
    fn availability(&self, pkg: &str) -> Availability {
        // apt-cache assente o in errore: nessuna informazione, non un verdetto.
        // Non si conclude nulla da qui — si prova la via lenta, e se anche
        // quella dice di no chi chiama incrocia con `index_is_queryable`.
        let policy_says_real =
            capture_command_with_env("apt-cache", &["policy", "--", pkg], &[("LC_ALL", "C")])
                .map(|out| has_installable_candidate(&out))
                .unwrap_or(false);

        // La via lenta si percorre **solo** se serve: `-s` = simulate, apt
        // calcola la soluzione senza toccare il sistema, ma fa girare il
        // risolutore (~0.4s). Esce 100 con "E: Unable to locate package" se il
        // nome non esiste, 0 se è installabile — anche quando è virtuale con un
        // solo fornitore.
        let resolver_accepts = !policy_says_real
            && run_apt(&["install", "-s", "-y", "--no-install-recommends", "--", pkg]).is_ok();

        availability_from(policy_says_real, resolver_accepts)
    }

    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = install_args(pkgs);
        run_apt(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// `apt-get purge`: rimuove i pacchetti indicati **e i loro file di
    /// configurazione**, senza toccare nient'altro. apt non rimuove le orfane se
    /// non glielo si chiede, quindi l'invariante di [`PackageManager::remove`] è
    /// soddisfatta senza opzioni aggiuntive.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = remove_args(pkgs);
        run_apt(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn remove_orphans(&self) -> Result<(), StepError> {
        run_apt(&["autoremove", "-y"])
    }

    /// `apt-get install -f -y`: installa le dipendenze mancanti e completa la
    /// configurazione dei pacchetti rimasti a metà, riportando `dpkg` in stato
    /// consistente.
    fn try_repair(&self) -> Result<(), StepError> {
        run_apt(&["install", "-f", "-y"])
    }

    /// `dpkg --configure -a`: riconfigura i pacchetti scompattati ma non
    /// configurati. Copre il caso in cui `apt-get install -f` non basta perché
    /// apt stesso si rifiuta di operare.
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
    /// Sostituisce `dpkg -i`, che installa il pacchetto ma **non** risolve le
    /// dipendenze: su un sistema minimale il `.deb` resta `unconfigured`, `dpkg`
    /// esce con errore e da lì in poi ogni comando apt fallisce, rollback
    /// compreso (A-RT-1/A-RT-2, dalla prova reale su Multipass).
    fn install_local_file(&self, path: &Path) -> Result<(), StepError> {
        // `--` prima del path: un `.deb` in una directory il cui nome inizia
        // con `-` non deve diventare un'opzione (stessa rete di R1).
        let rendered = path.to_string_lossy();
        run_apt(&["install", "-y", "--", &rendered])
    }

    fn catalog(&self) -> PackageCatalog {
        PackageCatalog {
            bootstrap: bootstrap_catalog(),
            odoo: odoo_catalog(),
            postgres: POSTGRES_PACKAGES.iter().map(|s| s.to_string()).collect(),
            postgres_marker: POSTGRES_MARKER_PACKAGE.to_string(),
            nginx: NGINX_PACKAGE.to_string(),
        }
    }
}
