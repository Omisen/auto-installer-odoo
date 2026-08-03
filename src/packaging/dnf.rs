//! Il backend **dnf/rpm**: comandi e nomi della famiglia Fedora.
//!
//! # Cosa è tarato su una macchina reale e cosa no
//!
//! I **comandi** sono documentati e stabili. I **nomi dei pacchetti** no: sono
//! la traduzione della lista Debian, e ~26 nomi su 30 divergono. Il progetto ha
//! una regola su questo — un rifiuto senza prova blocca il caso buono
//! (A5.1-bis) — ma vale anche il contrario: un nome sbagliato qui **blocca
//! l'installazione nello snapshot**, prima di mutare, con il gruppo irrisolto
//! nel messaggio. È il comportamento giusto, e significa che la prima Fedora
//! reale fallirà più volte prima di installare.
//!
//! La taratura non richiede installazioni: `sudo odoo-installer --dry-run`
//! esegue lo `snapshot` di ogni step senza toccare nulla, e
//! `unavailable_packages_error` riporta **tutti** i gruppi irrisolvibili in un
//! solo messaggio. Il ciclo è «dry-run → correggi → ripeti», di minuti.
//!
//! Ogni voce del catalogo marcata `// DA VERIFICARE` è un nome che non è stato
//! confermato su una Fedora vera.

use std::path::Path;

use super::{availability_from, Availability, CatalogEntry, DepId, PackageCatalog, PackageManager};
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// Prerequisiti bootstrap sulla famiglia Fedora.
fn bootstrap_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        CatalogEntry::new(DepId::Wget, &["wget"]),
        // `gettext-base` di Debian è la parte runtime (`envsubst`); su Fedora
        // sta nel pacchetto unico `gettext`.
        CatalogEntry::new(DepId::Gettext, &["gettext"]),
    ]
}

/// Dipendenze di sistema di Odoo sulla famiglia Fedora.
///
/// Tradotte dalla lista Debian, **non ancora verificate su una Fedora reale**:
/// vedi il doc del modulo per la procedura di taratura.
fn odoo_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        CatalogEntry::new(DepId::Wget, &["wget"]),
        CatalogEntry::new(DepId::PythonPip, &["python3-pip"]),
        CatalogEntry::new(DepId::PythonDev, &["python3-devel"]),
        // Su Fedora **non esiste** un `python3-venv`: il modulo `venv` è nella
        // libreria standard e `ensurepip` sta in `python3-libs`, che è già
        // presente su qualunque sistema con python3. La voce c'è lo stesso — il
        // bisogno esiste — e risolverà quasi sempre ad `AlreadyInstalled`, senza
        // gonfiare il delta.
        //
        // La verifica vera non è comunque questa: è la precondizione di
        // `create-virtualenv`, che chiede `import ensurepip` al Python del
        // sistema (A-R6-1). Un pacchetto in lista non prova che il modulo ci sia.
        CatalogEntry::new(DepId::PythonVenv, &["python3-libs"]),
        CatalogEntry::new(DepId::PythonWheel, &["python3-wheel"]),
        CatalogEntry::new(DepId::PythonSetuptools, &["python3-setuptools"]),
        // `build-essential` non ha equivalente: è un metapacchetto Debian. Su
        // Fedora l'analogo è il gruppo `@development-tools`, che però ha una
        // sintassi propria e un comportamento poco chiaro alla rimozione (il
        // delta non saprebbe cosa reclamare). Meglio i tre nomi espliciti: sono
        // ciò che serve davvero a compilare le estensioni native di pip.
        CatalogEntry::many(DepId::BuildTools, &["gcc", "gcc-c++", "make"]),
        CatalogEntry::new(DepId::Gettext, &["gettext"]),
        CatalogEntry::new(DepId::Freetype, &["freetype-devel"]),
        CatalogEntry::new(DepId::Xml2, &["libxml2-devel"]),
        CatalogEntry::new(DepId::Zip, &["libzip-devel"]),
        // Nome del tutto diverso: la libreria è OpenLDAP.
        CatalogEntry::new(DepId::Ldap, &["openldap-devel"]),
        // Idem: l'implementazione SASL è Cyrus.
        CatalogEntry::new(DepId::Sasl, &["cyrus-sasl-devel"]),
        // I tre nomi jpeg di Debian collassano su uno solo.
        CatalogEntry::new(DepId::Jpeg, &["libjpeg-turbo-devel"]),
        // Stesso pacchetto di `Jpeg`: la deduplica di A-MD-1 lo assorbe, e su
        // questa famiglia il duplicato è la norma, non un caso di bordo.
        CatalogEntry::new(DepId::Jpeg8, &["libjpeg-turbo-devel"]),
        // Cade anche il `1g` del soname.
        CatalogEntry::new(DepId::Zlib, &["zlib-devel"]),
        CatalogEntry::new(DepId::PostgresClient, &["libpq-devel"]),
        // Cade anche l'`1`.
        CatalogEntry::new(DepId::Xslt, &["libxslt-devel"]),
        CatalogEntry::new(DepId::Tiff, &["libtiff-devel"]),
        CatalogEntry::new(DepId::OpenJpeg, &["openjpeg2-devel"]),
        CatalogEntry::new(DepId::Lcms2, &["lcms2-devel"]),
        CatalogEntry::new(DepId::Webp, &["libwebp-devel"]),
        CatalogEntry::new(DepId::Harfbuzz, &["harfbuzz-devel"]),
        CatalogEntry::new(DepId::Fribidi, &["fribidi-devel"]),
        CatalogEntry::new(DepId::Xcb, &["libxcb-devel"]),
        CatalogEntry::new(DepId::Ev, &["libev-devel"]),
        CatalogEntry::new(DepId::CAres, &["c-ares-devel"]),
        // Opzionale come su Debian, e con la stessa ragione: Odoo moderno usa
        // SCSS. DA VERIFICARE se esista ancora su Fedora recente — ma un
        // opzionale mancante è un warning, non uno stop.
        CatalogEntry::optional(DepId::LessCompiler, &["nodejs-less"]),
    ]
}

/// Pacchetti che installano il server PostgreSQL su Fedora.
///
/// **Non** `postgresql`, che è il solo client: su Fedora il server è un
/// pacchetto a parte, e installare il client soltanto porterebbe a un
/// `systemctl start postgresql` che fallisce senza dire perché.
pub const POSTGRES_PACKAGES: &[&str] = &["postgresql-server", "postgresql-contrib"];
/// Il nome con cui si chiede «PostgreSQL è installato?» su Fedora.
///
/// È il **server**, non il client: `postgresql` è installato anche su una
/// macchina che ha solo `psql`, e prenderlo come marker farebbe credere che il
/// server ci sia già — quindi `Preexisting`, quindi nessuno stop e nessun undo.
pub const POSTGRES_MARKER_PACKAGE: &str = "postgresql-server";
/// Il pacchetto di nginx (identico su entrambe le famiglie).
pub const NGINX_PACKAGE: &str = "nginx";

/// Gli argomenti di `dnf install`, come funzione **pura**.
///
/// Estratta perché il flag che conta non è verificabile altrimenti: il codice
/// che esegue `dnf` gira solo su una Fedora vera, e senza questa separazione
/// «qualcuno toglie `install_weak_deps=False`» sarebbe una modifica che nessun
/// test può vedere. È lo stesso motivo per cui esiste `availability_from`.
pub fn install_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "install".to_string(),
        "-y".to_string(),
        // Controparte di `--no-install-recommends`: senza, dnf tira dentro i
        // `Recommends` e il delta cresce di pacchetti che nessuno ha chiesto —
        // e che l'undo poi rimuoverebbe.
        "--setopt=install_weak_deps=False".to_string(),
        "--".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// Gli argomenti di `dnf remove`, come funzione **pura**.
///
/// Vedi [`DnfBackend::remove`] per il perché di
/// `clean_requirements_on_remove=False`: è la condizione perché la promessa
/// chirurgica valga su questa famiglia, e va verificata dove si può.
pub fn remove_args(pkgs: &[&str]) -> Vec<String> {
    let mut args = vec![
        "remove".to_string(),
        "-y".to_string(),
        "--setopt=clean_requirements_on_remove=False".to_string(),
        "--".to_string(),
    ];
    args.extend(pkgs.iter().map(|p| p.to_string()));
    args
}

/// Il gestore di pacchetti della famiglia Fedora.
#[derive(Debug, Default)]
pub struct DnfBackend;

/// Esegue `dnf` in modo non interattivo.
///
/// Non serve l'equivalente di `DEBIAN_FRONTEND`: dnf non fa domande con `-y`, e
/// non esiste un `needrestart` da mettere a tacere.
fn run_dnf(args: &[&str]) -> Result<(), StepError> {
    run_command("dnf", args)
}

impl PackageManager for DnfBackend {
    fn is_installed(&self, pkg: &str) -> bool {
        std::process::Command::new("rpm")
            .args(["-q", "--", pkg])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    /// `dnf makecache`.
    ///
    /// Su dnf è **meno necessario** che su apt — i metadati scadono da soli
    /// (`metadata_expire`, default 48h) e vengono riscaricati alla prima
    /// operazione che li richiede. Si esegue lo stesso, e per la stessa ragione
    /// per cui esiste su apt: A5.1-bis nasceva da un indice vecchio che faceva
    /// rispondere «questo pacchetto non esiste» a una domanda la cui risposta
    /// era «non lo so». Meglio pagare qualche secondo che diagnosticare
    /// un'assenza inventata.
    fn refresh_index(&self) -> Result<(), StepError> {
        run_dnf(&["makecache"])
    }

    /// C'è almeno un repository abilitato da cui ottenere risposte?
    ///
    /// L'analogo di `apt-cache stats`: distingue **cecità** da **assenza**. Su
    /// Fedora il caso concreto non è «l'indice non è mai stato aggiornato» ma
    /// «i repository non sono raggiungibili», e l'effetto sulla diagnosi è lo
    /// stesso — ogni nome risulterebbe inesistente.
    fn index_is_queryable(&self) -> bool {
        capture_command("dnf", &["repolist", "--enabled", "--quiet"])
            .map(|out| out.lines().any(|line| !line.trim().is_empty()))
            .unwrap_or(false)
    }

    /// Due domande, come su apt, ma con comandi diversi.
    ///
    /// 1. `dnf list --available --installed <pkg>` risponde solo se **quel
    ///    nome** è un pacchetto vero: è l'equivalente del candidato reale, cioè
    ///    di un nome che dopo l'installazione `rpm -q` saprà riconoscere.
    /// 2. se no, resta da distinguere «non esiste» da «lo fornisce qualcun
    ///    altro»: `dnf install --assumeno` fa girare il risolutore senza
    ///    installare, ed esce 1 con «Nothing to do» per un nome ignoto ma
    ///    risolve un `Provides`.
    ///
    /// La politica che ne discende — reale batte virtuale — è la stessa di apt e
    /// vive in `AptPackagesStep::resolve`: qui c'è solo il meccanismo.
    fn availability(&self, pkg: &str) -> Availability {
        let real = capture_command("dnf", &["list", "--quiet", "--", pkg])
            .map(|out| {
                out.lines().any(|line| {
                    line.split_whitespace()
                        .next()
                        .is_some_and(|first| first == pkg || first.starts_with(&format!("{pkg}.")))
                })
            })
            .unwrap_or(false);

        // La via lenta si percorre **solo** se serve, come su apt.
        let resolver_accepts = !real
            && run_dnf(&[
                "install",
                "--assumeno",
                "--setopt=install_weak_deps=False",
                "--",
                pkg,
            ])
            .is_ok();

        availability_from(real, resolver_accepts)
    }

    /// `dnf install -y`, **senza dipendenze deboli**.
    ///
    /// `--setopt=install_weak_deps=False` è la controparte di
    /// `--no-install-recommends`: senza, dnf tira dentro i `Recommends` e il
    /// delta cresce di pacchetti che nessuno ha chiesto — e che l'undo poi
    /// rimuoverebbe.
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = install_args(pkgs);
        run_dnf(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// `dnf remove -y`, **senza toccare le orfane**.
    ///
    /// # `clean_requirements_on_remove=False` non è un dettaglio
    ///
    /// È la condizione perché la promessa chirurgica valga anche qui. Il default
    /// di dnf è rimuovere anche le dipendenze diventate inutili: sarebbe
    /// esattamente l'`apt-get autoremove` globale che R0 ha **bandito**
    /// dall'undo perché non è delimitato dal nostro delta. Su apt quella
    /// rimozione è un'azione esplicita, confinata a `--aggressive-rollback`
    /// ([`Self::remove_orphans`]); qui accadrebbe di default, in ogni rollback,
    /// e potrebbe portarsi via una libreria condivisa con software del cliente.
    ///
    /// Il flag si passa **sempre**, anche se il default un giorno cambiasse: un
    /// comportamento su cui poggia una promessa non si lascia decidere a un file
    /// di configurazione che non controlliamo.
    ///
    /// # Cosa resta, dichiarato
    ///
    /// Su rpm non esiste il `purge` di deb. Un file di configurazione
    /// **modificato** rispetto a quello del pacchetto viene rinominato in
    /// `.rpmsave` invece di essere cancellato. Per il delta pesante di Odoo —
    /// una trentina di pacchetti `-devel`, che file di configurazione non ne
    /// hanno — il residuo atteso è **nessuno**; per `postgresql-server` e
    /// `nginx`, che si rimuovono solo con `--aggressive-rollback`, può esserci.
    /// È un residuo inerte e tracciabile, della stessa categoria del log
    /// dell'installer, che pure sopravvive al rollback per scelta.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError> {
        let args = remove_args(pkgs);
        run_dnf(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    fn remove_orphans(&self) -> Result<(), StepError> {
        run_dnf(&["autoremove", "-y"])
    }

    /// **No-op**, e non per pigrizia.
    ///
    /// `try_repair` esiste perché apt si rifiuta di operare su un `dpkg` lasciato
    /// a metà — lo stato «scompattato ma non configurato», che
    /// `apt-get install -f` risolve (A-RT-2). In rpm quello stato **non esiste**:
    /// una transazione o è applicata o è annullata, e non c'è nulla di analogo da
    /// riparare. Gli equivalenti più vicini (`dnf history undo`, `rpm --rebuilddb`)
    /// fanno cose diverse: il primo è una seconda semantica di rollback accanto
    /// alla nostra, il secondo ricostruisce il database e non riguarda i
    /// pacchetti a metà.
    ///
    /// La politica di `steps::remove_with_recovery` degrada quindi in «prova a
    /// rimuovere, riprova una volta, poi elenca i residui» — e la parte che conta
    /// davvero, il **report dei residui**, resta identica per tutte le famiglie.
    fn try_repair(&self) -> Result<(), StepError> {
        Ok(())
    }

    /// No-op, per la stessa ragione di [`Self::try_repair`].
    fn try_deep_repair(&self) -> Result<(), StepError> {
        Ok(())
    }

    /// `dnf install -y <path.rpm>`: installa un `.rpm` locale risolvendone le
    /// dipendenze, come `apt-get install <file.deb>` sul lato Debian.
    fn install_local_file(&self, path: &Path) -> Result<(), StepError> {
        let rendered = path.to_string_lossy();
        run_dnf(&["install", "-y", "--", &rendered])
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
