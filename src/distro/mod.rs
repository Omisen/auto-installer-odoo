//! La **famiglia** della distribuzione, e la regola per cui si rilegge invece di
//! rideducersi.
//!
//! # Perché esiste un tipo, e non basta l'`ID` di `os-release`
//!
//! `checks::OsInfo` porta `id`, `version` e `codename`: tre stringhe grezze. Va
//! benissimo per applicare le soglie di versione, ma non risponde alla domanda
//! che il resto del programma dovrà porsi — *«con quali comandi si installa e si
//! rimuove un pacchetto su questa macchina?»* — perché quella domanda non
//! riguarda `ubuntu` o `debian` singolarmente, riguarda la **famiglia** a cui
//! appartengono.
//!
//! Oggi la nozione esiste già, ma implicita e locale: `install_wkhtmltopdf`
//! sceglie il pacchetto di ripiego con un `match os_id { Some("debian") => …,
//! _ => … }`. Un tipo la rende una cosa sola, nominata una volta.
//!
//! # La regola che questo tipo serve a rendere possibile
//!
//! **La famiglia si RILEGGE dal manifesto, non si rideduce dal sistema.**
//!
//! È la stessa regola per cui il verdetto «il database era nostro» viene copiato
//! nello snapshot di `setup-data-dir` invece di essere ricalcolato, e per cui il
//! rollback da disco non riesegue gli `snapshot()`: un'informazione che c'era al
//! momento della mutazione non va dedotta di nuovo dopo, perché nel frattempo il
//! sistema è cambiato — o non è nemmeno lo stesso sistema.
//!
//! Il caso concreto che si vuole evitare è il rollback da disco. Il `Context`
//! che il comando `rollback` ricostruisce dal manifesto ha `os_info: None`
//! (documentato in `state.rs`: «serve solo a `run`»), e non era falso finché
//! nessun `undo` dipendeva dall'OS. Con due gestori di pacchetti diventerebbe il
//! difetto ricorrente di questo progetto — *un'informazione che c'era e non è
//! stata letta* — in una forma particolarmente cattiva: l'undo del delta
//! pacchetti non saprebbe quale comando invocare.
//!
//! Le alternative sono state valutate e scartate, entrambe perché **deducono**:
//! ri-rilevare l'OS all'avvio del rollback (ma il manifesto potrebbe descrivere
//! un'installazione fatta altrove, o l'OS potrebbe essere stato aggiornato), e
//! auto-rilevare il gestore da quale binario esiste (su una macchina con
//! entrambi darebbe la risposta sbagliata **in silenzio**).

pub mod debian;
pub mod fedora;
pub mod firewalld;
pub mod ufw;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::StepError;

/// Il confine **convenzioni di distribuzione**, secondo dei due.
///
/// Astrae ciò che diverge fra famiglie *senza* essere packaging: dove stanno i
/// file, quali concetti esistono, quale strumento governa il firewall. Si
/// ottiene dal confine esistente con
/// [`SystemOps::distro`](crate::system_ops::SystemOps::distro), come il gestore
/// di pacchetti: non è una seconda porta verso il sistema.
///
/// # Perché è separato da [`crate::packaging`]
///
/// Perché sono divergenze di **natura** diversa. «Con quale comando si installa
/// un pacchetto» e «in quale directory nginx cerca i vhost» non hanno nulla in
/// comune se non il fatto di dipendere dalla distribuzione: unirle darebbe
/// un'astrazione che astrae due cose, e il primo che deve aggiungere una riga
/// non saprebbe da che parte guardare.
///
/// # Cresce a fasi, e non prima di avere consumatori
///
/// Oggi espone solo il firewall. `layout()` (percorsi nginx) e
/// `init_postgres_cluster()` arrivano con le fasi che li usano: un metodo senza
/// chiamanti è codice che nessun test può esercitare, e in questo progetto è
/// così che nascono i rami che non possono fallire.
/// Dove questa famiglia tiene le cose. **Dati**, non comportamento.
///
/// # Perché `Option` e non stringhe diverse
///
/// Perché due delle divergenze non sono «un percorso diverso» ma «**il concetto
/// non esiste**». Su Fedora `sites-enabled` non ha un altro nome: non c'è, e il
/// server di default non è un file separato — è un blocco `server` dentro
/// `/etc/nginx/nginx.conf`.
///
/// Un `None` lo dice; una costante che puntasse a una directory inventata
/// mentirebbe, e lo step andrebbe a creare symlink in un posto che nginx non
/// legge. La differenza va **rappresentata nei dati**, non nascosta in un ramo.
#[derive(Debug, Clone)]
pub struct NginxLayout {
    /// Dove si scrive il vhost.
    pub vhost_dir: PathBuf,
    /// Estensione che il file deve avere per essere caricato.
    ///
    /// Vuota su Debian (`sites-enabled/*` include qualunque file); `.conf` su
    /// Fedora, dove `nginx.conf` include `conf.d/*.conf` — **solo** quelli. Un
    /// vhost senza estensione lì sarebbe invisibile, e nulla lo direbbe.
    pub vhost_extension: &'static str,
    /// La directory dei siti **abilitati**, se il concetto esiste.
    pub enabled_dir: Option<PathBuf>,
    /// Il default site come file separato, se il concetto esiste.
    pub default_site: Option<PathBuf>,
    /// Il target *standard di distribuzione* del default site.
    ///
    /// Serve **solo** come ripiego per gli stati persistiti prima della R11, che
    /// registravano l'esistenza ma non il target.
    pub default_site_standard_target: Option<PathBuf>,
    /// Dove finisce il backup di un default site che è un **file regolare**.
    ///
    /// Deliberatamente fuori da `enabled_dir`: nginx include quella directory
    /// con un glob, quindi un backup lasciato lì verrebbe caricato lo stesso e
    /// la porta 80 resterebbe occupata — lo stesso difetto con un altro nome.
    pub default_site_backup_dir: PathBuf,
}

impl NginxLayout {
    /// Il percorso del vhost per questa versione di Odoo.
    pub fn vhost_path(&self, version_short: &str) -> PathBuf {
        self.vhost_dir
            .join(format!("odoo{version_short}{}", self.vhost_extension))
    }

    /// Il percorso del symlink che abilita il sito, se la famiglia ha il concetto.
    pub fn enabled_link(&self, version_short: &str) -> Option<PathBuf> {
        self.enabled_dir
            .as_ref()
            .map(|dir| dir.join(format!("odoo{version_short}{}", self.vhost_extension)))
    }
}

pub trait Distro {
    /// Lo strumento di firewall di questa famiglia.
    fn firewall(&self) -> &dyn Firewall;

    /// Dove questa famiglia tiene la configurazione di nginx.
    fn nginx_layout(&self) -> NginxLayout;

    /// Dove vive il cluster PostgreSQL, **se** questa famiglia richiede di
    /// inizializzarlo a mano. `None` = il pacchetto lo crea e lo avvia da sé.
    ///
    /// # La divergenza più pesante fra le due famiglie
    ///
    /// Su Debian/Ubuntu il postinst di `postgresql` chiama `pg_createcluster` e
    /// il servizio parte: non c'è nulla da fare, e infatti `setup-postgres` non
    /// ha mai avuto un passo di inizializzazione. Su Fedora
    /// `postgresql-server` **non inizializza niente**: senza
    /// `postgresql-setup --initdb` il servizio non parte, e lo step fallirebbe
    /// alla verifica finale senza dire perché.
    ///
    /// `Option` e non una stringa vuota: «questa famiglia non ha il concetto» è
    /// una risposta diversa da «il percorso è questo», e va rappresentata nei
    /// dati invece che dedotta.
    fn postgres_data_dir(&self) -> Option<PathBuf>;

    /// Inizializza il cluster PostgreSQL.
    ///
    /// Chiamata **solo** quando [`Self::postgres_data_dir`] è `Some` e il
    /// cluster non c'è ancora. Sulle famiglie che non ne hanno bisogno è un
    /// no-op totale: non un ramo irraggiungibile, ma la risposta vera alla
    /// domanda «cosa serve fare qui?».
    fn init_postgres_cluster(&self) -> Result<(), StepError>;
}

/// Lo strumento di firewall, in cinque domande.
///
/// La mappatura è 1:1 sui comandi, e il **token della regola è lo stesso** sulle
/// due famiglie: `ufw allow 80/tcp` e `firewall-cmd --add-port=80/tcp` accettano
/// la stessa stringa, e la elencano nella stessa forma. Per questo lo step
/// `nginx-firewall` — cioè il pattern delta, cioè la protezione — non cambia di
/// una riga quando cambia lo strumento sotto.
///
/// È un trait e non un gruppo di costanti perché ciò che diverge sono i
/// **comandi** e il loro modello (firewalld distingue runtime e permanente, e ha
/// le zone): una costante non saprebbe esprimerlo, e comprimerlo in un metodo
/// che «di solito» fa la cosa giusta è il tipo di scorciatoia da cui è nato
/// A-V3-7.
pub trait Firewall {
    /// Lo strumento è installato?
    fn available(&self) -> bool;
    /// Lo strumento è **attivo**? Se non lo è, non tocchiamo il firewall.
    fn is_active(&self) -> bool;
    /// La regola è già presente? (Se sì, non è nostra e l'undo non la toccherà.)
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError>;
    /// Apre la regola.
    fn allow(&self, rule: &str) -> Result<(), StepError>;
    /// Richiude la regola. Chiamata **solo** sul delta.
    fn delete(&self, rule: &str) -> Result<(), StepError>;
}

/// La famiglia di distribuzione a cui appartiene il sistema.
///
/// Raggruppa le distribuzioni che condividono gestore di pacchetti e convenzioni
/// (percorsi di nginx, strumento di firewall, inizializzazione del cluster
/// PostgreSQL). **Non** è la distribuzione: `ubuntu` e `debian` sono due `ID`
/// diversi della stessa famiglia, e dove la differenza conta — le soglie di
/// versione, il suffisso del pacchetto wkhtmltopdf — si continua a guardare
/// l'`id`, che resta in [`crate::checks::OsInfo`].
///
/// # Il default è `Debian`, e non è una comodità
///
/// Serve alla retrocompatibilità del manifesto: uno stato scritto prima che
/// questo campo esistesse non lo dichiara, e va letto come `Debian` perché
/// **ogni installazione esistente è apt**. È la stessa cura per cui
/// `InstallConfig` è `Option` dalla R4 e il percorso storico dello state resta
/// leggibile dalla R7: rendere illeggibile un manifesto significa rendere non
/// disinstallabile un'istanza.
///
/// Perché il default non diventi una bugia silenziosa, il rollback **logga** la
/// famiglia con cui sta lavorando e la confronta con il sistema sotto di sé
/// (vedi [`family_mismatch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OsFamily {
    /// Debian e derivate (Ubuntu): `apt`/`dpkg`, `sites-available`, `ufw`.
    #[default]
    Debian,
    /// Fedora: `dnf`/`rpm`, `conf.d`, `firewalld`.
    Fedora,
}

impl OsFamily {
    /// Deriva la famiglia dall'`ID` dichiarato in `/etc/os-release`.
    ///
    /// `None` = distribuzione che non sappiamo trattare. È l'**unico** posto in
    /// cui si decide se una distribuzione ci è nota: `checks::check_os_from` la
    /// consulta e rifiuta, così la stessa decisione non vive in due punti che
    /// possono divergere.
    ///
    /// # Perché non si legge `ID_LIKE`
    ///
    /// Rocky, AlmaLinux e CentOS Stream dichiarano `ID_LIKE=fedora`, e accettarlo
    /// le farebbe entrare **senza che nessuno le abbia mai provate**. Per una
    /// famiglia nuova si parte chiusi.
    ///
    /// Non è in contraddizione con A5.1-bis («un rifiuto senza prova blocca il
    /// caso buono»): lì si trattava di non respingere una release *più recente*
    /// di una famiglia già supportata, e infatti le soglie verso l'alto restano
    /// aperte — vedi [`crate::checks::validate_os`]. Qui si tratta di una
    /// distribuzione diversa, su cui non abbiamo alcuna prova in nessuna
    /// direzione.
    pub fn from_os_id(id: &str) -> Option<Self> {
        match id {
            "ubuntu" | "debian" => Some(OsFamily::Debian),
            "fedora" => Some(OsFamily::Fedora),
            _ => None,
        }
    }

    /// Nome stabile della famiglia, usato nei log e nel manifesto.
    pub fn as_str(&self) -> &'static str {
        match self {
            OsFamily::Debian => "debian",
            OsFamily::Fedora => "fedora",
        }
    }
}

impl std::fmt::Display for OsFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Il manifesto e il sistema sotto di noi concordano sulla famiglia?
///
/// Restituisce il testo dell'avviso quando **non** concordano, `None` quando
/// concordano o quando il sistema non è identificabile.
///
/// # Perché un avviso e non un rifiuto
///
/// La discordanza è possibile in due modi, e nessuno dei due giustifica di
/// fermarsi:
/// - un manifesto scritto prima che il campo esistesse ricade sul default
///   `Debian` (ed è quasi sempre la verità);
/// - `--state` accetta qualunque percorso, quindi si può ispezionare da una
///   macchina il manifesto di un'altra.
///
/// Rifiutare renderebbe **non disinstallabile** un'istanza, che è il danno di
/// A-V3-1 per un'altra strada. Dedurre dal sistema violerebbe la regola di
/// questo modulo. Resta l'avviso — e in pratica il caso degenera da sé: un
/// `apt-get purge` su un sistema senza apt fallisce, l'undo è best-effort
/// (invariante 3) e i residui finiscono nel report, che è esattamente ciò per
/// cui il report esiste.
///
/// Pura, e con il sistema come **parametro**: il caso interessante — manifesto
/// Debian su una macchina Fedora — non è riproducibile sulla macchina che esegue
/// i test. Stesso motivo di `checks::ensure_root_euid`, `checks::ports_to_check`
/// e `state::trust_verdict`.
pub fn family_mismatch(recorded: OsFamily, detected: Option<OsFamily>) -> Option<String> {
    let detected = detected?;
    if detected == recorded {
        return None;
    }
    Some(format!(
        "il manifesto è stato scritto da un'installazione su '{recorded}', ma questo sistema \
         è '{detected}'. Procedo con '{recorded}', che è ciò che il manifesto registra: gli \
         artefatti da rimuovere sono i suoi, non quelli che questa macchina suggerirebbe. \
         Se i comandi di rimozione falliscono, il report elencherà cosa è rimasto."
    ))
}
