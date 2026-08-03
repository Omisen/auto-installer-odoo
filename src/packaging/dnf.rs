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
//! # Stato della taratura
//!
//! **Verificata su Fedora 41 (dnf5 5.2.17)**, con `--dry-run`: tutti i 31 gruppi
//! si risolvono. Tre nomi si sono rivelati **virtuali** e sono stati corretti con
//! il nome reale come alternativa preferita — `wget`, `zlib-devel`,
//! `openjpeg2-devel`. È la stessa cura di `libfreetype6-dev` su Ubuntu 24.04
//! (A5.1-bis): un nome virtuale è installabile ma non rimovibile, e un delta che
//! lo contiene mente al rollback.
//!
//! Restano da provare su una macchina vera l'**installazione** e il **rollback**:
//! il dry-run risolve i nomi, non li installa.

use std::path::Path;

use super::{availability_from, Availability, CatalogEntry, DepId, PackageCatalog, PackageManager};
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// Prerequisiti bootstrap sulla famiglia Fedora.
fn bootstrap_catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::new(DepId::Git, &["git"]),
        CatalogEntry::new(DepId::Curl, &["curl"]),
        // `wget` su Fedora 41 **non è un pacchetto**: è un `Provides` di
        // `wget1-wget` (il wget classico) e `wget2-wget`. Verificato in campo.
        // Il nome reale va per primo — vedi la nota su `DepId::Wget` in
        // `odoo_catalog`.
        CatalogEntry::new(DepId::Wget, &["wget1-wget", "wget2-wget", "wget"]),
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
        // # Perché tre alternative e non un nome solo (verificato in campo)
        //
        // Su Fedora 41 `wget` è un nome **puramente virtuale**: `rpm -q wget`
        // non lo conosce, e `dnf remove wget` uscirebbe 0 rimuovendo zero
        // pacchetti. Un delta che lo contiene mente, e il rollback dichiarerebbe
        // rimosso ciò che è rimasto: è A5.1-bis, la stessa forma che su Ubuntu
        // 24.04 ha prodotto `libfreetype6-dev`.
        //
        // `wget1-wget` (il wget classico 1.x) va per primo perché è quello le cui
        // opzioni `-q -O` sono quelle che `RealDownloader` usa da sempre;
        // `wget2-wget` è il ripiego se il primo non c'è. Il nome virtuale resta
        // in coda come rete: se un domani Fedora rinominasse ancora, la
        // risoluzione lo troverebbe comunque — con il warning che dice perché.
        CatalogEntry::new(DepId::Wget, &["wget1-wget", "wget2-wget", "wget"]),
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
        // Cade il `1g` del soname — ma non basta: su Fedora 41 `zlib-devel` è a
        // sua volta **virtuale**, perché la distribuzione è migrata a `zlib-ng`
        // e il pacchetto reale è `zlib-ng-compat-devel` (verificato in campo).
        CatalogEntry::new(DepId::Zlib, &["zlib-ng-compat-devel", "zlib-devel"]),
        CatalogEntry::new(DepId::PostgresClient, &["libpq-devel"]),
        // Cade anche l'`1`.
        CatalogEntry::new(DepId::Xslt, &["libxslt-devel"]),
        CatalogEntry::new(DepId::Tiff, &["libtiff-devel"]),
        // Il `2` nel nome è storia: il pacchetto reale è `openjpeg-devel`, che
        // fornisce `openjpeg2-devel` per compatibilità (verificato in campo).
        CatalogEntry::new(DepId::OpenJpeg, &["openjpeg-devel", "openjpeg2-devel"]),
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
///
/// # Niente `--` prima dei nomi, e va dichiarato
///
/// Su apt il separatore `--` è **metà** della doppia difesa contro
/// l'argument injection (R1): l'altra metà è il validatore che pretende un primo
/// carattere alfanumerico. **dnf5 non lo accetta**: `dnf install -- <pkg>`
/// risponde `Unknown argument "--"` ed esce 2 — verificato su Fedora 41,
/// dnf5 5.2.17.
///
/// Su questa famiglia resta quindi una difesa sola. La superficie reale è nulla —
/// i nomi arrivano dal catalogo, che è fatto di costanti nel sorgente, non da
/// input dell'utente — ma un vincolo esterno che indebolisce una difesa va
/// scritto, non lasciato scoprire a chi legge il codice fra un anno.
fn run_dnf(args: &[&str]) -> Result<(), StepError> {
    run_command("dnf", args)
}

impl PackageManager for DnfBackend {
    fn is_installed(&self, pkg: &str) -> bool {
        // `rpm` accetta `--`, a differenza di dnf5: qui la doppia difesa regge.
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

    /// Due domande, come su apt, ma poste a `dnf repoquery` — il comando pensato
    /// per lo scripting.
    ///
    /// 1. **`repoquery --qf '%{name}'`**: esiste un pacchetto con *questo nome
    ///    esatto*? È l'equivalente del candidato reale, cioè di un nome che dopo
    ///    l'installazione `rpm -q` saprà riconoscere — e che quindi l'undo saprà
    ///    rimuovere.
    /// 2. **`repoquery --whatprovides`**: se no, qualcuno lo *fornisce*? Su
    ///    Fedora 41 `wget` è esattamente questo: non è un pacchetto, è fornito da
    ///    `wget1-wget` e `wget2-wget`.
    ///
    /// # Perché non i comandi ovvi (verificato in campo, Fedora 41 / dnf5 5.2.17)
    ///
    /// Il primo tentativo usava `dnf list` e `dnf install --assumeno`, e **nessuno
    /// dei due funzionava**:
    ///
    /// - `dnf list --quiet -- <pkg>` esce 2 con `Unknown argument "--"`: dnf5 non
    ///   accetta il separatore;
    /// - `dnf install --assumeno <pkg>` esce **2 anche quando il pacchetto
    ///   esiste**, perché l'operazione è stata annullata — che è precisamente ciò
    ///   che gli si era chiesto di fare.
    ///
    /// Il secondo è la firma ricorrente di questo progetto nella sua forma
    /// speculare: non un controllo che non può fallire, ma uno **che non può
    /// riuscire**. L'effetto era che ogni pacchetto non già installato risultava
    /// assente, e il primo dry-run su Fedora si è fermato elencando ventiquattro
    /// nomi che esistevano tutti.
    ///
    /// `repoquery` non ha il problema: risponde con l'elenco — vuoto se nulla
    /// corrisponde — ed esce **0** in entrambi i casi. Alla domanda si risponde
    /// leggendo l'output, non l'exit code. È la stessa lezione di R9-hotfix:
    /// *`exit != 0` non dice PERCHÉ*.
    ///
    /// La politica che ne discende — reale batte virtuale — è la stessa di apt e
    /// vive in `AptPackagesStep::resolve`: qui c'è solo il meccanismo.
    fn availability(&self, pkg: &str) -> Availability {
        let real = capture_command("dnf", &["repoquery", "--quiet", "--qf", "%{name}\n", pkg])
            .map(|out| out.lines().any(|line| line.trim() == pkg))
            .unwrap_or(false);

        // La via lenta si percorre **solo** se serve, come su apt.
        let provided_by_others = !real
            && capture_command("dnf", &["repoquery", "--quiet", "--whatprovides", pkg])
                .map(|out| out.lines().any(|line| !line.trim().is_empty()))
                .unwrap_or(false);

        availability_from(real, provided_by_others)
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
    /// # Cosa il flag NON impedisce (verificato in campo)
    ///
    /// Le **reverse dependencies**. `dnf remove git` su Fedora 41 annuncia anche
    /// `Removing dependent packages: perl-Git`, e non è il flag a governarlo: è
    /// obbligatorio, perché rpm non può lasciare installato un pacchetto la cui
    /// dipendenza sparisce. `apt-get purge` fa lo stesso, quindi **non è una
    /// divergenza fra famiglie** — ma è un limite della promessa chirurgica che
    /// vale per entrambe e che va detto: rimuovere un pacchetto del nostro delta
    /// può portarsi via un pacchetto **del cliente** che dipendeva da lui.
    ///
    /// Non c'è modo di evitarlo restando dentro il gestore, e uscirne
    /// (`rpm -e --nodeps`) lascerebbe il sistema in uno stato che il gestore non
    /// sa più riparare — molto peggio del residuo che si vorrebbe evitare.
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
        run_dnf(&["install", "-y", &rendered])
    }

    /// Lo schema rpm di upstream: `wkhtmltox-{ver}.{suffisso}.x86_64.rpm`.
    ///
    /// **DA VERIFICARE su una release reale**: il nome è ricostruito dalla
    /// convenzione rpm, non letto dagli asset pubblicati. Se fosse sbagliato il
    /// download fallirebbe con un 404 — rumoroso, non silenzioso — e comunque
    /// prima del download c'è il fail-closed sui pin, che per questa famiglia
    /// non esistono ancora.
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
        }
    }
}
