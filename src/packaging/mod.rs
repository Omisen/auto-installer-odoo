//! Il confine **gestore di pacchetti**: cosa sa fare, e quali nomi conosce.
//!
//! È il primo dei due confini del supporto multi-distro (l'altro è
//! [`crate::distro`], le convenzioni di distribuzione). Non è una seconda porta
//! verso il sistema: si **ottiene** dal confine esistente, con
//! [`SystemOps::packages`](crate::system_ops::SystemOps::packages). Gli step
//! smettono di dire `ops.apt_install(...)` e dicono `ops.packages().install(...)`;
//! ciò che tocca il sistema resta un posto solo, e i test continuano a mockarne
//! uno solo.
//!
//! # Perché un trait e non un `enum`
//!
//! Con due sole famiglie un `enum` con un `match` in ogni metodo sarebbe più
//! diretto, e l'argomento «i backend in file separati» da solo non basterebbe a
//! deciderlo. A decidere è **il mock**: se
//! [`SystemOps::packages`](crate::system_ops::SystemOps::packages)
//! restituisse un enum, il mock dei test dovrebbe restituire un'istanza di quel
//! tipo — i cui rami eseguono `apt-get` e `dnf` per davvero. Per poterlo mockare
//! servirebbe una terza variante di test **dentro il tipo di produzione**, cioè
//! un ramo che in produzione non può eseguire: la firma ricorrente dei difetti
//! di questo progetto, introdotta di proposito. Il `dyn` non costa nulla di
//! nuovo (`SystemOps` è già `Box<dyn>` ovunque, e il programma è I/O-bound).
//!
//! # Cosa **non** sta qui
//!
//! Nginx, firewall e convenzioni di percorso: sono divergenza *di
//! distribuzione*, non *di packaging*, e stanno in [`crate::distro`]. Mescolarle
//! darebbe un'astrazione che astrae due cose diverse — e il nome «backend
//! multi-distro» le nasconde, il che è la ragione per cui vanno nominate a parte.
//!
//! Nemmeno la **politica di rollback**: `steps::remove_with_recovery` resta uno
//! step-level helper, perché il confine è una mappatura 1:1 sui comandi e i test
//! devono poter asserire la sequenza esatta.

pub mod apt;
pub mod dnf;

use std::path::Path;

use crate::error::StepError;

/// Che cosa sa dirci il gestore su un nome di pacchetto.
///
/// Tre valori e non due booleani, e la distinzione è quella conquistata sul
/// campo con A5.1-bis. Il punto è separare due cose che oggi sono impastate:
///
/// - il **meccanismo** è del gestore. Su apt servono due comandi, perché
///   `apt-cache policy` dice `Candidate: (none)` su un nome puramente virtuale
///   mentre `apt-get install -s` lo installerebbe benissimo.
/// - la **politica** — «un nome reale batte un nome virtuale» — non dipende
///   dalla famiglia: un nome che il gestore non riconoscerà più dopo
///   l'installazione non è rimovibile, e un delta che lo contiene mente. Vale
///   su rpm come su deb.
///
/// Con due booleani il chiamante dovrebbe sapere in che ordine interrogarli e
/// perché, cioè il meccanismo di apt trapelerebbe nella politica. Con un enum la
/// politica diventa una funzione **pura** su tre valori (vedi
/// `AptPackagesStep::resolve`) e ogni backend risponde onestamente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Installabile, e dopo l'installazione il gestore lo riconosce con
    /// **questo** nome: è rimovibile, quindi il delta che lo contiene è onesto.
    Real,
    /// Installabile, ma solo perché un altro pacchetto lo *fornisce*: dopo
    /// l'installazione il gestore non conosce questo nome, e rimuoverlo non
    /// rimuove niente. Utilizzabile come **ripiego**, mai come prima scelta.
    VirtualOnly,
    /// Non installabile con questo nome — oppure il gestore non è in grado di
    /// rispondere. Le due cose si distinguono con
    /// [`PackageManager::index_is_queryable`], non tirando a indovinare.
    Absent,
}

/// La decisione di «che disponibilità ha questo nome», sui soli esiti osservati.
///
/// Pura, e separata dai comandi che quegli esiti li producono. È il pattern che
/// questo progetto usa dove una decisione conta più del modo in cui si ottengono
/// i suoi input (`checks::ensure_root_euid`, `checks::ports_to_check`,
/// `state::trust_verdict`, `distro::ufw::rule_in_status`): il codice che esegue
/// `apt-cache` e `apt-get` può essere verificato **solo** su una macchina reale,
/// e senza questa separazione la regola che conta resterebbe fuori da ogni test.
///
/// L'ordine è la protezione di A5.1-bis: **un candidato reale batte sempre un
/// nome virtuale**, perché un nome virtuale non è rimovibile e un delta che lo
/// contiene mente.
pub fn availability_from(policy_says_real: bool, resolver_accepts: bool) -> Availability {
    if policy_says_real {
        Availability::Real
    } else if resolver_accepts {
        Availability::VirtualOnly
    } else {
        Availability::Absent
    }
}

/// Un requisito di pacchetto: uno o più nomi **alternativi**, in ordine di
/// preferenza, che soddisfano lo stesso bisogno.
///
/// # L'invariante da non perdere con due famiglie
///
/// Le alternative sono «stesso bisogno, nomi diversi **sulla stessa famiglia**»:
/// `libtiff5-dev` e `libtiff-dev` sono lo stesso pacchetto su due release
/// Debian. Mettere `freetype-devel` accanto a `libfreetype6-dev` sembrerebbe
/// gratis e **romperebbe il gruppo**: la prima regola della risoluzione — «vince
/// un'alternativa già installata» — è corretta fra sinonimi della stessa distro
/// e diventa una trappola fra nomi di famiglie diverse; e la diagnostica
/// mostrerebbe a chi sta su Fedora un gruppo in cui due nomi su tre non lo
/// riguardano.
///
/// Perciò la famiglia **non** entra nel gruppo: entra un livello sopra, in
/// [`PackageCatalog`], che è ciò che il backend risponde quando gli si chiede
/// l'elenco.
///
/// Possiede le `String` invece di prendere `&'static str` perché i test
/// costruiscono gruppi a runtime; le liste di produzione restano `const` (vedi
/// [`apt::ODOO_DEPENDENCIES`]) e passano da [`PackageSpec::group`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    alternatives: Vec<String>,
    /// `false` = se nessuna alternativa è installabile si prosegue senza.
    required: bool,
}

impl PackageSpec {
    /// Un solo nome, nessuna alternativa: se manca, lo step si ferma.
    pub fn one(name: &str) -> Self {
        Self::any(&[name])
    }

    /// Alternative in ordine di preferenza (la prima è il nome preferito).
    pub fn any(alternatives: &[&str]) -> Self {
        PackageSpec {
            alternatives: alternatives.iter().map(|s| s.to_string()).collect(),
            required: true,
        }
    }

    /// Come [`PackageSpec::any`], ma un gruppo interamente non disponibile è un
    /// warning e non un errore.
    pub fn optional(alternatives: &[&str]) -> Self {
        PackageSpec {
            required: false,
            ..Self::any(alternatives)
        }
    }

    /// Converte un gruppo delle liste `const` in un `PackageSpec` obbligatorio.
    pub fn group(group: &[&str]) -> Self {
        Self::any(group)
    }

    /// Le alternative, in ordine di preferenza.
    pub fn alternatives(&self) -> &[String] {
        &self.alternatives
    }

    /// `true` se un gruppo senza alternative disponibili deve fermare lo step.
    pub fn is_required(&self) -> bool {
        self.required
    }

    /// Il nome preferito (il primo del gruppo), per i messaggi diagnostici.
    /// Un gruppo vuoto non è costruibile dalle liste di produzione; se ci
    /// arrivasse comunque, qui vale `"<gruppo vuoto>"` invece di panicare.
    pub fn preferred(&self) -> &str {
        self.alternatives
            .first()
            .map(String::as_str)
            .unwrap_or("<gruppo vuoto>")
    }
}

/// Converte una lista canonica (`&[&[&str]]`) in specs obbligatori.
pub fn specs(groups: &[&[&str]]) -> Vec<PackageSpec> {
    groups.iter().map(|g| PackageSpec::group(g)).collect()
}

/// Un **bisogno** dell'installazione, indipendente dal nome che ha su una
/// distribuzione.
///
/// # A cosa serve, e a cosa NON serve
///
/// Non serve alla risoluzione: quella continua a lavorare su nomi, come sempre.
/// Serve a **una cosa sola**: un test che enumera queste varianti e pretende che
/// ogni famiglia le copra tutte.
///
/// La lezione di R6-hotfix-2 era «congela la lista, così un refactor che perde un
/// pacchetto lo dice subito». Con due famiglie la lezione si estende: non basta
/// che ogni lista sia congelata, serve che le due si **corrispondano**. Senza,
/// si aggiunge una dipendenza a Debian e ci si accorge che manca su Fedora solo
/// quando una VM non compila più — cioè nel posto più caro possibile.
///
/// # Perché la corrispondenza non è 1:1, e va bene così
///
/// Un bisogno può costare **più pacchetti** su una famiglia (`BuildTools` è un
/// pacchetto su Debian e tre su Fedora) e **lo stesso** pacchetto su due bisogni
/// diversi (`Jpeg` e `Jpeg8` collassano entrambi su `libjpeg-turbo-devel`: la
/// deduplica di A-MD-1 se ne occupa). Per questo una voce di catalogo porta un
/// `Vec<PackageSpec>` e non uno solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepId {
    Git,
    Curl,
    Wget,
    /// `envsubst`, per rendere i template.
    Gettext,
    PythonPip,
    PythonDev,
    /// Il modulo `ensurepip`, senza cui `python3 -m venv` si ferma a metà
    /// (A-R6-1). Su alcune famiglie è un pacchetto a sé, su altre sta nella
    /// libreria standard: il bisogno c'è comunque, ed è la precondizione di
    /// `create-virtualenv` a verificarlo davvero.
    PythonVenv,
    PythonWheel,
    PythonSetuptools,
    /// Compilatore C/C++ e make: servono a compilare le estensioni native di pip.
    BuildTools,
    Freetype,
    Xml2,
    Zip,
    Ldap,
    Sasl,
    Jpeg,
    /// Variante storica del precedente: su alcune release è un pacchetto di
    /// transizione, su altre non esiste e ricade sullo stesso nome di [`Self::Jpeg`].
    Jpeg8,
    Zlib,
    /// Header del client PostgreSQL (`libpq`), per `psycopg2`.
    PostgresClient,
    Xslt,
    Tiff,
    OpenJpeg,
    Lcms2,
    Webp,
    Harfbuzz,
    Fribidi,
    Xcb,
    Ev,
    CAres,
    /// **Opzionale**: il compilatore degli asset `.less`. Odoo moderno usa SCSS
    /// e parte senza; se manca è un warning, non un errore.
    LessCompiler,
}

impl DepId {
    /// Tutti i bisogni, per il test di parità fra cataloghi.
    pub const ALL: &'static [DepId] = &[
        DepId::Git,
        DepId::Curl,
        DepId::Wget,
        DepId::Gettext,
        DepId::PythonPip,
        DepId::PythonDev,
        DepId::PythonVenv,
        DepId::PythonWheel,
        DepId::PythonSetuptools,
        DepId::BuildTools,
        DepId::Freetype,
        DepId::Xml2,
        DepId::Zip,
        DepId::Ldap,
        DepId::Sasl,
        DepId::Jpeg,
        DepId::Jpeg8,
        DepId::Zlib,
        DepId::PostgresClient,
        DepId::Xslt,
        DepId::Tiff,
        DepId::OpenJpeg,
        DepId::Lcms2,
        DepId::Webp,
        DepId::Harfbuzz,
        DepId::Fribidi,
        DepId::Xcb,
        DepId::Ev,
        DepId::CAres,
        DepId::LessCompiler,
    ];
}

/// Un bisogno e i pacchetti che lo soddisfano **su questa famiglia**.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub id: DepId,
    /// Uno o più `PackageSpec`: un bisogno può costare più pacchetti.
    pub specs: Vec<PackageSpec>,
}

impl CatalogEntry {
    /// Una voce con un solo gruppo di alternative.
    pub fn new(id: DepId, alternatives: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: vec![PackageSpec::any(alternatives)],
        }
    }

    /// Una voce **opzionale**: se nessuna alternativa è disponibile si prosegue.
    pub fn optional(id: DepId, alternatives: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: vec![PackageSpec::optional(alternatives)],
        }
    }

    /// Una voce che costa **più pacchetti** (es. `build-essential` → gcc, g++, make).
    pub fn many(id: DepId, packages: &[&str]) -> Self {
        CatalogEntry {
            id,
            specs: packages.iter().map(|p| PackageSpec::one(p)).collect(),
        }
    }
}

/// I nomi di pacchetto che una famiglia conosce.
///
/// # Perché la lista sta nel backend e non in una tabella a parte
///
/// Perché la lista **è** conoscenza del gestore: dire «su dnf i nomi sono
/// questi» non è diverso da dire «su dnf si installa così». Tenerle separate
/// significherebbe avere due posti da aggiornare quando si aggiunge una
/// dipendenza, e la lezione di R6-hotfix-2 è che le liste vanno protette dai
/// refactor, non moltiplicate.
///
/// Sono **dati**, non metodi: così restano leggibili in blocco (per capire cosa
/// installiamo su una distro basta aprire un file) e congelabili da un test.
#[derive(Debug, Clone)]
pub struct PackageCatalog {
    /// Utility comuni a basso rischio, installate per prime.
    pub bootstrap: Vec<CatalogEntry>,
    /// Dipendenze di sistema di Odoo: obbligatorie **e** opzionali insieme,
    /// perché `PackageSpec` porta già con sé la distinzione.
    pub odoo: Vec<CatalogEntry>,
    /// Pacchetti che installano il server PostgreSQL.
    pub postgres: Vec<String>,
    /// Il nome con cui si chiede «PostgreSQL è installato?». **Non** è il primo
    /// elemento di `postgres`: è una domanda diversa, e su Fedora la risposta è
    /// un nome diverso (`postgresql-server`, non `postgresql`).
    pub postgres_marker: String,
    /// Il pacchetto di nginx.
    pub nginx: String,
}

impl PackageCatalog {
    /// Le specs del bootstrap, appiattite: è ciò che lo step consuma.
    pub fn bootstrap_specs(&self) -> Vec<PackageSpec> {
        Self::flatten(&self.bootstrap)
    }

    /// Le specs delle dipendenze Odoo, appiattite.
    pub fn odoo_specs(&self) -> Vec<PackageSpec> {
        Self::flatten(&self.odoo)
    }

    fn flatten(entries: &[CatalogEntry]) -> Vec<PackageSpec> {
        entries.iter().flat_map(|e| e.specs.clone()).collect()
    }

    /// Questo catalogo copre il bisogno? (bootstrap **o** dipendenze Odoo)
    pub fn covers(&self, id: DepId) -> bool {
        self.bootstrap
            .iter()
            .chain(self.odoo.iter())
            .any(|e| e.id == id && !e.specs.is_empty())
    }
}

/// I comandi di un gestore di pacchetti.
///
/// La superficie è deliberatamente **piccola e 1:1 sui comandi**: nessuna
/// politica qui dentro, così i test possono asserire la sequenza esatta e
/// `steps::remove_with_recovery` resta l'unico posto in cui vive la strategia di
/// rimozione con recupero.
pub trait PackageManager {
    /// Il pacchetto risulta installato **con questo nome**?
    ///
    /// La precisazione conta: un nome puramente virtuale risponde `false` anche
    /// subito dopo essere stato «installato», ed è la ragione per cui
    /// [`Availability::VirtualOnly`] è un ripiego.
    fn is_installed(&self, pkg: &str) -> bool;

    /// Riscarica gli indici dei repository.
    ///
    /// È una mutazione (tocca la cache del gestore), quindi vive **solo** dentro
    /// un `run` — mai in uno `snapshot`, che non muta per invariante (C4). Non
    /// ha undo: un indice aggiornato non cambia nulla di ciò che è installato, è
    /// la cache di ciò che *si potrebbe* installare. Come un `git fetch`.
    fn refresh_index(&self) -> Result<(), StepError>;

    /// L'indice è interrogabile, cioè le risposte di [`Self::availability`]
    /// significano qualcosa?
    ///
    /// Serve a non confondere **cecità** con **assenza**: su una macchina dove
    /// l'indice non è mai stato aggiornato ogni interrogazione risponde «non
    /// disponibile», e senza questa domanda un indice vuoto diventerebbe la
    /// diagnosi «questo pacchetto non esiste su questa release» — il falso
    /// positivo A5.1-bis, che in campo ha mandato a cercare la rinomina di un
    /// pacchetto che stava benissimo al suo posto.
    fn index_is_queryable(&self) -> bool;

    /// Che cosa sa dirci il gestore su questo nome. Query, non mutazione.
    fn availability(&self, pkg: &str) -> Availability;

    /// Installa (idempotente), senza raccomandati/dipendenze deboli.
    fn install(&self, pkgs: &[&str]) -> Result<(), StepError>;

    /// Rimuove **esattamente** i pacchetti indicati.
    ///
    /// Si chiama `remove` e non `purge` di proposito: «purge» è un concetto deb,
    /// e il nome di un metodo non deve promettere una semantica che una delle
    /// implementazioni non ha.
    ///
    /// **L'invariante che ogni implementazione deve rispettare**: rimuovere solo
    /// ciò che è stato chiesto. Nessuna rimozione di dipendenze diventate
    /// orfane — quella è [`Self::remove_orphans`], che è un'azione separata e
    /// confinata a `--aggressive-rollback`. Su un gestore che lo fa di default,
    /// va **disattivato esplicitamente**: sarebbe l'`autoremove` globale che R0
    /// ha bandito dall'undo perché non è delimitato dal nostro delta, cioè
    /// l'esatto contrario del principio chirurgico.
    fn remove(&self, pkgs: &[&str]) -> Result<(), StepError>;

    /// Rimuove le dipendenze diventate orfane. **Solo** `--aggressive-rollback`.
    fn remove_orphans(&self) -> Result<(), StepError>;

    /// Tenta di riportare il database dei pacchetti in stato consistente.
    ///
    /// Il rollback arriva sempre *dopo* un fallimento, e quel fallimento può
    /// aver lasciato il gestore a metà (A-RT-2: su dpkg rotto, apt si rifiuta di
    /// operare e **ogni** purge del rollback fallisce). Su gestori che non hanno
    /// uno stato «scompattato ma non configurato» è legittimo che sia un no-op.
    fn try_repair(&self) -> Result<(), StepError>;

    /// Il secondo livello di recupero, per quando [`Self::try_repair`] non basta
    /// perché il gestore stesso si rifiuta di operare.
    fn try_deep_repair(&self) -> Result<(), StepError>;

    /// Installa un pacchetto da un **file locale**, risolvendone le dipendenze.
    ///
    /// Il path dev'essere assoluto (o iniziare con `./`): i gestori trattano
    /// come file solo gli argomenti che contengono una `/`, altrimenti li cercano
    /// nei repository.
    fn install_local_file(&self, path: &Path) -> Result<(), StepError>;

    /// Come si chiama il file di pacchetto di questo formato.
    ///
    /// Serve a `install-wkhtmltopdf`, che scarica un pacchetto **da upstream**:
    /// il progetto pubblica sia `.deb` sia `.rpm`, con schemi di nome diversi
    /// (`wkhtmltox_{ver}.{suffisso}_amd64.deb` contro
    /// `wkhtmltox-{ver}.{suffisso}.x86_64.rpm`). Non è una convenzione nostra,
    /// ma è conoscenza del **formato di pacchetto**, quindi di chi lo installa.
    ///
    /// L'estensione conta oltre che per il nome: R9 ha scoperto che
    /// `apt-get install <file>` riconosce un percorso locale **solo** da quella,
    /// e per questo il temporaneo con nome casuale la conserva
    /// (`private_temp_path_keeping_extension`).
    fn local_package_name(&self, version: &str, suffix: &str) -> String;

    /// Il comando che l'utente digiterebbe per aggiornare l'indice, **come
    /// testo**, da mettere nei messaggi diagnostici.
    ///
    /// Serve a non scrivere «esegui `apt-get update`» a chi sta su Fedora. Un
    /// suggerimento sbagliato è peggio di nessun suggerimento: manda a provare
    /// un comando che non esiste e fa dubitare del resto della diagnosi.
    fn refresh_command(&self) -> &'static str;

    /// I nomi di pacchetto che questa famiglia conosce.
    fn catalog(&self) -> PackageCatalog;
}
