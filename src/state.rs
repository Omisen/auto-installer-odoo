//! Pattern `PreState` e persistenza dello stato su disco.
//!
//! Implementa l'invariante 4 di `CLAUDE.md`: `completed` + snapshot vanno
//! scritti su `/var/lib/odoo-installer/state.json` (owned root, `0600`).
//! Il percorso storico `/opt/odoo/.installer-state.json` resta **leggibile**
//! (vedi [`resolve_state_path`]) ma non viene più scritto: il manifesto non può
//! vivere dentro il perimetro che il rollback deve rimuovere, o l'ultimo undo
//! trova la directory occupata proprio da lui (A-V3-2).
//!
//! # Il consumatore dello stato: il comando `rollback` (R4)
//!
//! Il file di stato viene **scritto** dopo ogni step riuscito ([`InstallState::save`],
//! chiamata da [`crate::engine::Installer::execute_with_reporter`]),
//! **riletto** da `odoo-installer rollback` ([`InstallState::load`], vedi
//! [`crate::rollback`]) e **rimosso** a fine installazione riuscita
//! ([`InstallState::clear`], chiamata da `main`) o a rollback completato.
//!
//! Il rollback esiste quindi in due forme, con la stessa semantica:
//! - **in-process** — [`crate::engine::Installer`] annulla gli step completati
//!   nella *stessa* esecuzione quando uno step fallisce;
//! - **da disco** — [`crate::rollback::rollback_from_state`] ricostruisce gli
//!   step dai record persistiti e ne esegue gli `undo` in ordine inverso. È la
//!   via per ripulire dopo un Ctrl-C, un `kill -9` o un power-loss, e per
//!   disinstallare un'installazione riuscita.
//!
//! # Perché lo stato porta anche la configurazione
//!
//! Uno `StepRecord` dice *in che stato era* l'artefatto (il `PreState`), non
//! *quale* artefatto fosse: `CreateDatabase` serializza `CreatedByUs`, ma il
//! nome del database vive nel [`Context`]. Per il rollback in-process non è un
//! problema (il `Context` è lì); per il rollback da disco lo sarebbe.
//!
//! Ricavare la configurazione una seconda volta dalla cascata CLI/`.env`/default
//! **non è un'opzione sicura**: un utente che ha installato con
//! `--db-name fatturazione` e lancia `odoo-installer rollback` senza flag
//! ricadrebbe sul default `odoo` e il rollback droppererebbe un database che non
//! ha mai creato — la violazione più diretta della protezione anti-drop. Perciò
//! l'installazione persiste la propria configurazione ([`InstallConfig`])
//! insieme ai record: il rollback usa **quella**, cioè l'identità reale degli
//! artefatti creati.
//!
//! **Nessuna password è persistita**: l'undo non ne ha bisogno (drop di ruoli e
//! database avviene via `psql` come utente `postgres`), e un segreto non scritto
//! è un segreto che non può trapelare.
//!
//! Il path è configurabile (vedi [`crate::context::Context::state_path`]) così
//! i test girano senza root e senza toccare il sistema.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::error::StepError;

/// Directory del manifesto in produzione. Creata da [`InstallState::save`] e
/// rimossa, se vuota, da [`InstallState::clear`].
pub const DEFAULT_STATE_DIR: &str = "/var/lib/odoo-installer";

/// Percorso di default del file di stato in produzione (root, `0600`).
///
/// **Fuori da `/opt/odoo`** (A-V3-2). Il manifesto è l'ultimo artefatto a
/// morire: `clear` lo rimuove *dopo* che tutti gli undo sono girati, perché un
/// rollback che non arriva in fondo deve poter essere ripetuto. Finché viveva
/// dentro `/opt/odoo`, l'undo di `PrepareOptRoot` — che è l'**ultimo** a girare,
/// essendo il primo step — trovava lì il manifesto, vedeva una directory non
/// vuota e rinunciava a rimuoverla (mai `rm -rf`). Spostare lock e log non
/// bastava: era questo il terzo residente, ed è il motivo per cui `/opt/odoo`
/// sopravviveva comunque a ogni rollback.
pub const DEFAULT_STATE_PATH: &str = "/var/lib/odoo-installer/state.json";

/// Percorso storico del manifesto, dentro `/opt/odoo` (fino alla 2.1.0).
///
/// Non si scrive più qui, ma si continua a **leggere**: un'istanza installata
/// da una versione precedente ha il suo unico manifesto in questa posizione, e
/// rendergliela invisibile significherebbe renderla non disinstallabile — cioè
/// il danno che A-V3-1 descrive, causato dalla correzione di un altro finding.
/// Vedi [`resolve_state_path`].
pub const LEGACY_STATE_PATH: &str = "/opt/odoo/.installer-state.json";

/// Il file di stato è **affidabile** come sorgente di un'operazione distruttiva?
///
/// Il manifesto guida dei `rm -rf`, dei `dropdb` e dei `userdel`. Prima di
/// eseguirli va stabilito che quel file non sia sotto il controllo di qualcun
/// altro (A-V3-8): dev'essere di **root**, non scrivibile da gruppo o altri, e
/// deve stare in una directory a sua volta non scrivibile da terzi — altrimenti
/// chiunque potrebbe sostituirlo e scegliere lui cosa faremo sparire.
///
/// Non si applica al `--dry-run`, che stampa soltanto: lì poter ispezionare un
/// manifesto copiato altrove è comodo e non fa danni.
pub fn ensure_trustworthy(path: &Path) -> Result<(), StepError> {
    use std::os::unix::fs::MetadataExt;

    let meta = fs::metadata(path).map_err(|e| StepError::io(path, e))?;
    let parent_mode = path
        .parent()
        .and_then(|p| fs::metadata(p).ok())
        .map(|m| m.mode());

    trust_verdict(meta.uid(), meta.mode(), parent_mode).map_err(|reason| {
        StepError::Precondition(format!(
            "il file di stato {} non è una fonte affidabile: {reason}.\n\
             \n\
             Guida `rm -rf`, `dropdb` e `userdel`: non lo consumo da qualcosa che un \
             altro utente potrebbe riscrivere o sostituire.",
            path.display()
        ))
    })
}

/// La regola di [`ensure_trustworthy`], sui soli numeri.
///
/// Separata perché il caso **positivo** — file di root, `0600`, in una directory
/// non scrivibile da terzi — non è riproducibile in un test che gira senza
/// privilegi: un file creato in una tempdir appartiene all'utente che esegue i
/// test. Con i permessi come parametri la regola si verifica per intero, in
/// entrambe le direzioni. Stesso motivo per cui esistono `checks::ensure_root_euid`
/// e `checks::ensure_sudo_user`.
pub fn trust_verdict(uid: u32, mode: u32, parent_mode: Option<u32>) -> Result<(), String> {
    if uid != 0 {
        return Err(format!("non appartiene a root (uid {uid})"));
    }
    if mode & 0o022 != 0 {
        return Err(format!(
            "è scrivibile da gruppo o altri (mode {:o})",
            mode & 0o777
        ));
    }
    // La directory conta quanto il file: in una directory scrivibile da terzi il
    // file si sostituisce senza bisogno di poterlo modificare. Lo sticky bit —
    // `/tmp` — toglie proprio quella possibilità, quindi non è un problema.
    if let Some(dir_mode) = parent_mode {
        let sticky = dir_mode & 0o1000 != 0;
        if dir_mode & 0o022 != 0 && !sticky {
            return Err(format!(
                "sta in una directory scrivibile da terzi (mode {:o}), dove chiunque \
                 potrebbe sostituirlo",
                dir_mode & 0o777
            ));
        }
    }
    Ok(())
}

/// Sceglie il manifesto da consumare quando l'utente non passa `--state`.
///
/// Ordine: il percorso corrente se esiste, altrimenti quello storico se esiste,
/// altrimenti di nuovo il corrente (così il messaggio d'errore nomina il posto
/// giusto in cui l'utente dovrebbe cercarlo). Non tenta alcuna migrazione: un
/// rollback non è il momento per spostare file, e il manifesto storico verrà
/// comunque rimosso dal `clear` a pulizia completata.
pub fn resolve_state_path() -> PathBuf {
    pick_state_path(Path::new(DEFAULT_STATE_PATH), Path::new(LEGACY_STATE_PATH))
}

/// La regola di [`resolve_state_path`], con i due percorsi come parametri.
///
/// Esiste separata per la stessa ragione per cui `Context::state_path` è
/// configurabile: la scelta va verificata con percorsi di prova, non contro il
/// filesystem della macchina che esegue i test.
pub fn pick_state_path(current: &Path, legacy: &Path) -> PathBuf {
    if current.exists() {
        return current.to_path_buf();
    }
    if legacy.exists() {
        return legacy.to_path_buf();
    }
    current.to_path_buf()
}

/// Modalità del file di stato: leggibile/scrivibile solo dal proprietario.
const STATE_FILE_MODE: u32 = 0o600;

/// Stato preesistente di un artefatto rispetto all'installer.
///
/// È la sola fonte di verità per l'undo (invariante 1): `undo` agisce **solo**
/// se lo step è `completed` E `PreState == CreatedByUs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PreState {
    /// `run()` non ancora eseguito → nessun undo.
    #[default]
    Untracked,
    /// L'artefatto esisteva già prima di noi → undo NO-OP (non è nostro da
    /// distruggere).
    Preexisting,
    /// Creato da noi → undo rimuove.
    CreatedByUs,
}

/// Record di uno step completato con successo, persistito su disco.
///
/// Lo `snapshot` è un valore JSON opaco al motore: ogni step serializza qui il
/// proprio `PreState` (o una struttura più ricca). È l'informazione che
/// *servirà* a ricostruire l'undo in un'esecuzione successiva — oggi viene
/// scritta ma non ancora riletta (vedi la nota nel doc del modulo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub name: String,
    pub snapshot: serde_json::Value,
}

/// Configurazione dell'installazione persistita insieme ai record.
///
/// Contiene **l'identità degli artefatti** che gli `undo` devono poter
/// nominare: quale utente, quale database, quale directory. Senza, il rollback
/// da disco non saprebbe *cosa* rimuovere (vedi il doc del modulo).
///
/// # Cosa NON contiene, di proposito
///
/// Nessuna password (`admin_passwd`, `db_password`). Nessun `undo` ne ha
/// bisogno: il drop del ruolo e del database passa da `psql`/`dropdb` eseguiti
/// come utente `postgres`, non dall'autenticazione del ruolo Odoo. Persistere
/// un segreto che non serve sarebbe superficie d'attacco gratuita, per quanto il
/// file sia `0600` root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallConfig {
    pub odoo_version: String,
    pub odoo_version_short: String,
    pub odoo_user: String,
    pub db_user: String,
    pub db_name: String,
    pub odoo_home: PathBuf,
    pub install_dir: PathBuf,
    pub port: u16,
    /// `None` = Odoo logga su journal/stdout (nessuna log dir creata).
    pub odoo_logfile: Option<PathBuf>,
    pub with_nginx: bool,
    /// Utente che ha lanciato `sudo`: possiede control-script e `.bashrc`.
    pub sudo_user: Option<String>,
}

impl InstallConfig {
    /// Estrae dal [`Context`] i soli campi che servono al rollback da disco.
    pub fn from_context(ctx: &Context) -> Self {
        InstallConfig {
            odoo_version: ctx.odoo_version.clone(),
            odoo_version_short: ctx.odoo_version_short.clone(),
            odoo_user: ctx.odoo_user.clone(),
            db_user: ctx.db_user.clone(),
            db_name: ctx.db_name.clone(),
            odoo_home: ctx.odoo_home.clone(),
            install_dir: ctx.install_dir.clone(),
            port: ctx.port,
            odoo_logfile: ctx.odoo_logfile.clone(),
            with_nginx: ctx.with_nginx,
            sudo_user: ctx.sudo_user.clone(),
        }
    }

    /// Le due configurazioni nominano gli **stessi artefatti**?
    ///
    /// Serve al resume (A-V3-1): riprendere un'installazione interrotta con
    /// parametri diversi produrrebbe un manifesto che descrive un'istanza mai
    /// esistita — metà artefatti con un nome, metà con un altro — e gli undo
    /// punterebbero in parte altrove. Nel caso del database è la violazione
    /// diretta dell'anti-drop: si riprenderebbe con `--db-name` diverso e il
    /// rollback droppererebbe un database che non abbiamo creato.
    ///
    /// Si confrontano solo i campi che **identificano** un artefatto. Restano
    /// fuori, di proposito: `port` e `odoo_logfile` (nessun undo li usa per
    /// nominare qualcosa — il logfile è coperto dalla directory, che è un
    /// artefatto proprio di `SetupLogDir`), `with_nginx` (aggiunge step, non
    /// rinomina i precedenti) e `sudo_user` (chi riprende può legittimamente
    /// essere un altro amministratore).
    pub fn same_identity(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }

    /// I campi confrontati da [`same_identity`](InstallConfig::same_identity),
    /// etichettati, per poter **dire all'utente cosa** non coincide invece di
    /// un generico "parametri diversi".
    pub fn identity(&self) -> Vec<(&'static str, String)> {
        vec![
            ("versione Odoo", self.odoo_version.clone()),
            ("utente di sistema", self.odoo_user.clone()),
            ("ruolo database", self.db_user.clone()),
            ("nome database", self.db_name.clone()),
            ("home", self.odoo_home.display().to_string()),
            (
                "directory di installazione",
                self.install_dir.display().to_string(),
            ),
        ]
    }

    /// Il manifesto descrive un'installazione **di questo installer**?
    ///
    /// # Perché serve, e perché sta qui (A-V3-8)
    ///
    /// Gli `undo` cancellano alberi a partire da percorsi che arrivano dal file
    /// di stato, e `--state <FILE>` accetta qualunque percorso. La rete che
    /// c'era — `steps::remove_created_root`, che pretende `target` sotto `home`
    /// — non proteggeva da nulla, perché **`home` e `target` vengono entrambi
    /// dallo stesso file**: con `odoo_home: "/"` e `created_root: "/etc"` la
    /// guardia passava senza obiezioni.
    ///
    /// La correzione non è irrigidire quella guardia ma **ancorarla a un valore
    /// che non arriva dal file**: `ODOO_HOME` è dichiarata costante
    /// architetturale e non sovrascrivibile (`config.rs`), quindi un manifesto
    /// che ne dichiara un'altra non descrive un'installazione fatta da questo
    /// programma. Validare qui — al confine, quando i dati non fidati entrano —
    /// copre in un colpo solo *tutti* gli undo che usano `odoo_home`, non solo
    /// quell'unica funzione: la rimozione della home, il `chown` di ripristino,
    /// il filestore e la cache.
    pub fn validate_perimeter(&self) -> Result<(), StepError> {
        let atteso = Path::new(crate::config::ODOO_HOME);
        if self.odoo_home != atteso {
            return Err(StepError::Precondition(format!(
                "il manifesto dichiara come home '{}', ma questo installer usa solo '{}' \
                 (costante architetturale).\n\
                 \n\
                 Non descrive un'installazione fatta da questo programma, e gli undo \
                 agirebbero su percorsi che non conosciamo: mi fermo senza toccare nulla.",
                self.odoo_home.display(),
                atteso.display()
            )));
        }
        if !self.install_dir.starts_with(atteso) || self.install_dir == atteso {
            return Err(StepError::Precondition(format!(
                "il manifesto dichiara come directory di installazione '{}', che non sta \
                 sotto '{}': mi fermo senza toccare nulla.",
                self.install_dir.display(),
                atteso.display()
            )));
        }
        Ok(())
    }

    /// Ricostruisce il [`Context`] per il rollback da disco.
    ///
    /// I campi non persistiti restano ai default: le password sono vuote
    /// (nessun undo le usa) e `os_info` è `None` (serve solo a `run`).
    pub fn to_context(
        &self,
        dry_run: bool,
        aggressive_rollback: bool,
        state_path: PathBuf,
    ) -> Context {
        Context {
            odoo_version: self.odoo_version.clone(),
            odoo_version_short: self.odoo_version_short.clone(),
            odoo_user: self.odoo_user.clone(),
            db_user: self.db_user.clone(),
            db_name: self.db_name.clone(),
            odoo_home: self.odoo_home.clone(),
            install_dir: self.install_dir.clone(),
            port: self.port,
            odoo_logfile: self.odoo_logfile.clone(),
            with_nginx: self.with_nginx,
            sudo_user: self.sudo_user.clone(),
            dry_run,
            aggressive_rollback,
            state_path,
            ..Default::default()
        }
    }
}

/// Esito della decisione presa all'avvio di un'installazione (A-V3-1).
///
/// È una **politica pura**, come `rollback::confirmation_gate`: `main` la
/// applica e ne formatta i messaggi, ma la regola sta qui e si verifica senza
/// filesystem, senza root e senza terminale. Non è un vezzo — il difetto che
/// questa decisione chiude viveva in `main`, fra pezzi che i test coprivano
/// singolarmente, ed è precisamente il tipo di codice che questo progetto ha
/// imparato a non lasciare fuori dalla libreria.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartDecision {
    /// Nessun manifesto utile: prima installazione.
    Fresh,
    /// Manifesto parziale e compatibile: si riprende.
    Resume,
    /// `--force` su un manifesto esistente: si archivia e si riparte da capo.
    Replace,
    /// Manifesto di un'installazione **conclusa**: si rifiuta.
    RefuseFinished,
    /// Manifesto parziale che nomina artefatti diversi da quelli richiesti.
    /// Porta le differenze (campo, valore registrato, valore richiesto).
    RefuseIdentityMismatch(Vec<(&'static str, String, String)>),
    /// Manifesto parziale senza configurazione (formato pre-R4): non si può
    /// stabilire se descriva gli stessi artefatti, quindi non si riprende.
    RefuseUnknownIdentity,
}

/// La regola di avvio: installare, riprendere o rifiutare.
///
/// # Perché un manifesto non si sovrascrive mai in silenzio
///
/// Prima di R8 il percorso di installazione non apriva mai il manifesto:
/// `Installer::new()` e, al primo step, `save` che tronca. Su un'installazione
/// già conclusa il risultato era un manifesto in cui **niente è nostro** — gli
/// snapshot vedono correttamente ogni artefatto già presente — e da lì il
/// rollback eseguiva ventiquattro undo NO-OP, dichiarava «nessun residuo» e
/// cancellava lo stato: Odoo installato per sempre, senza più traccia di cosa
/// rimuovere.
///
/// Il caso **parziale** è la stessa perdita in forma più insidiosa, perché
/// colpisce il flusso che il progetto dichiara supportato («rilancia e
/// prosegui»): gli step del primo giro tornerebbero `Preexisting` e il database
/// creato allora finirebbe protetto dall'anti-drop. Per questo si riprende
/// invece di rifiutare — ma solo a parità di identità degli artefatti.
pub fn start_decision(
    state: &InstallState,
    requested: &InstallConfig,
    force: bool,
) -> StartDecision {
    if state.completed.is_empty() {
        return StartDecision::Fresh;
    }
    if force {
        return StartDecision::Replace;
    }
    if state.finished {
        return StartDecision::RefuseFinished;
    }

    let Some(precedente) = &state.config else {
        return StartDecision::RefuseUnknownIdentity;
    };

    let differenze: Vec<(&'static str, String, String)> = precedente
        .identity()
        .into_iter()
        .zip(requested.identity())
        .filter(|((_, prima), (_, ora))| prima != ora)
        .map(|((campo, prima), (_, ora))| (campo, prima, ora))
        .collect();

    if differenze.is_empty() {
        StartDecision::Resume
    } else {
        StartDecision::RefuseIdentityMismatch(differenze)
    }
}

/// Stato completo dell'installazione persistito su disco.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstallState {
    /// Step completati, in ordine di esecuzione. Il rollback li percorre in
    /// ordine inverso (invariante 2).
    pub completed: Vec<StepRecord>,
    /// Configurazione dell'installazione in corso, necessaria al rollback da
    /// disco per sapere *quali* artefatti annullare.
    ///
    /// `Option` + `#[serde(default)]` per retrocompatibilità: un file di stato
    /// scritto da una versione precedente a R4 non ha questo campo e resta
    /// leggibile. Il comando `rollback` lo rileva e si ferma con un messaggio
    /// esplicito invece di indovinare la configurazione — indovinare significa
    /// rischiare di droppare il database sbagliato.
    #[serde(default)]
    pub config: Option<InstallConfig>,
    /// `true` quando l'installazione è arrivata in fondo.
    ///
    /// Distingue le due cose che il file di stato può descrivere, e che
    /// richiedono messaggi (non comportamenti) diversi: **un'installazione
    /// funzionante da disinstallare** oppure **i residui di una run
    /// interrotta**. Vedi [`crate::rollback::install_status`].
    ///
    /// Dedurlo dal numero di step non basta: la sequenza canonica cambia fra
    /// versioni, e uno stato completo scritto da una versione con meno step
    /// verrebbe riletto come "interrotto". Il flag lo dice e basta.
    #[serde(default)]
    pub finished: bool,
}

impl InstallState {
    /// Carica lo stato dal file. Un file assente equivale a stato vuoto: è la
    /// condizione normale di una prima esecuzione, non un errore.
    ///
    /// È il punto d'ingresso del rollback da disco
    /// ([`crate::rollback::rollback_from_state`]).
    pub fn load(path: &Path) -> Result<Self, StepError> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                StepError::io(
                    path,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StepError::io(path, e)),
        }
    }

    /// Scrive lo stato su disco creando il file con permessi `0600` fin dalla
    /// creazione (nessuna finestra a permessi larghi). Se il file esisteva già,
    /// i permessi vengono comunque forzati a `0600`.
    pub fn save(&self, path: &Path) -> Result<(), StepError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| StepError::io(parent, e))?;
            }
        }

        let json = serde_json::to_vec_pretty(self).map_err(|e| {
            StepError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::InvalidData, e),
            )
        })?;

        // `mode()` applica i permessi solo alla *creazione* del file.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(STATE_FILE_MODE)
            .open(path)
            .map_err(|e| StepError::io(path, e))?;
        file.write_all(&json).map_err(|e| StepError::io(path, e))?;

        // Se il file preesisteva con permessi diversi, `mode()` non li tocca:
        // forziamo `0600` esplicitamente.
        fs::set_permissions(path, fs::Permissions::from_mode(STATE_FILE_MODE))
            .map_err(|e| StepError::io(path, e))?;

        Ok(())
    }

    /// Rimuove il file di stato. Idempotente: un file già assente non è errore.
    ///
    /// Chiamata **solo** dal comando `rollback` a pulizia completata: lì lo
    /// stato ha davvero esaurito il suo scopo, perché ciò che descriveva non
    /// esiste più.
    ///
    /// # Perché NON a fine installazione riuscita (A-R5-1)
    ///
    /// Fino a R4 `main` chiamava `clear` a successo avvenuto, per non lasciare
    /// un file "stantìo". Il ragionamento veniva da quando l'unico consumatore
    /// ipotizzato era un *resume*, per il quale uno stato completo sarebbe
    /// stato effettivamente spazzatura. Ma il consumatore che è stato
    /// implementato è il *rollback*, e per lui uno stato completo non è
    /// spazzatura: è il **manifesto di disinstallazione**, l'unica traccia di
    /// quali artefatti quell'installazione ha creato e quali ha trovato già lì.
    /// Cancellandolo si rendeva impossibile proprio il caso d'uso principale del
    /// comando — `odoo-installer rollback` su un'istanza funzionante rispondeva
    /// "nessuna installazione da annullare".
    ///
    /// Ora a fine successo lo stato viene **marcato** ([`InstallState::finished`])
    /// e conservato.
    /// # Perché rimuove anche la directory
    ///
    /// `save` crea `/var/lib/odoo-installer` per ospitare il manifesto: è un
    /// artefatto nostro, e lasciarne il guscio vuoto dopo una pulizia riuscita
    /// sarebbe lo stesso residuo che A-V3-2 rimprovera a `/opt/odoo`, solo più
    /// piccolo. La rimozione è ristretta alla **costante** [`DEFAULT_STATE_DIR`]
    /// e alla sola directory vuota: un `--state` che punti altrove non fa
    /// sparire la directory di qualcun altro.
    pub fn clear(path: &Path) -> Result<(), StepError> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(StepError::io(path, e)),
        }

        if path.parent() == Some(Path::new(DEFAULT_STATE_DIR)) {
            // Best-effort e mai forzata: `remove_dir` fallisce da sé se dentro
            // c'è rimasto qualcosa, ed è il comportamento voluto.
            let _ = fs::remove_dir(DEFAULT_STATE_DIR);
        }

        Ok(())
    }

    /// Aggiunge un record di step completato allo stato in memoria.
    pub fn record(&mut self, record: StepRecord) {
        self.completed.push(record);
    }

    /// Registra la configurazione dell'installazione (una sola volta, prima del
    /// primo step): è ciò che permette al rollback da disco di sapere *quali*
    /// artefatti annullare.
    pub fn set_config(&mut self, config: InstallConfig) {
        self.config = Some(config);
    }

    /// Il record di uno step già completato, se c'è. È il punto d'appoggio del
    /// resume (A-V3-1): il motore lo usa per sapere che quello step **è già
    /// stato eseguito**, e ne reidrata lo snapshot invece di rifarlo.
    ///
    /// La ricerca è per **nome**, non per posizione: la sequenza canonica può
    /// cambiare fra versioni dell'installer, e un manifesto scritto da una
    /// versione con meno step non deve essere riletto per indice — è lo stesso
    /// motivo per cui esiste il flag `finished`.
    pub fn record_for(&self, name: &str) -> Option<&StepRecord> {
        self.completed.iter().find(|r| r.name == name)
    }

    /// La porta HTTP è occupata da un servizio **di questa installazione**?
    ///
    /// Vero quando il manifesto registra `setup-systemd` come completato: da quel
    /// momento il servizio Odoo è installato e attivo, quindi è lui a tenere la
    /// porta.
    ///
    /// Serve al **resume** (A-R9-1): il controllo di preflight sulla porta esiste
    /// per intercettare un conflitto con *qualcun altro*. In un resume, se il
    /// primo giro era arrivato oltre `setup-systemd`, quel qualcun altro siamo
    /// noi — e rifiutare l'esecuzione renderebbe irriprendibile proprio
    /// l'installazione che stiamo riprendendo. Se invece `setup-systemd` non era
    /// passato, la porta è di un terzo e il conflitto è reale: il controllo va
    /// fatto.
    ///
    /// Si legge dal manifesto e non si deduce dal sistema, per la stessa ragione
    /// per cui il rollback rilegge i `PreState` invece di rifare gli snapshot:
    /// "chi tiene la porta" non è osservabile, "chi l'ha aperta" sì — ed è
    /// scritto.
    pub fn owns_the_http_port(&self) -> bool {
        self.record_for("setup-systemd").is_some()
    }
}
