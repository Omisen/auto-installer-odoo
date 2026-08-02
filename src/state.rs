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
}
