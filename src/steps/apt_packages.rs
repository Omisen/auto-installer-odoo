//! [`AptPackagesStep`]: installa un insieme di pacchetti apt, in modo
//! reversibile, applicando il **pattern delta**.
//!
//! È la generalizzazione del `PreState` singolo a un *insieme*: lo snapshot non
//! è più "esiste sì/no" ma "quale sottoinsieme era già installato prima di me".
//! L'undo rimuove **solo il delta** — i pacchetti che NON c'erano prima e che
//! quindi abbiamo aggiunto noi — mai i preesistenti.
//!
//! Due configurazioni (una lista in un solo posto, come `_apt_packages_odoo`):
//! - [`AptPackagesStep::bootstrap`] — utility comuni (git/curl/wget/gettext).
//!   Undo **non purga** di default (decisione D3): non si disinstalla git/curl
//!   da una macchina cliente per un rollback. Purga solo con
//!   `--aggressive-rollback`.
//! - [`AptPackagesStep::odoo_dependencies`] — i ~30 pacchetti dev di Odoo. È il
//!   **delta pesante**: l'undo purga il delta, e **solo** il delta (nessun
//!   `apt-get autoremove` globale — vedi `purge_delta`, A3.2).
//!
//! # Nomi di pacchetto portabili (A5.1)
//!
//! Lo stesso pacchetto non ha lo stesso nome su tutte le release: `libtiff5-dev`
//! è `libtiff-dev` su Debian recente, `libjpeg8-dev` non esiste affatto su
//! Debian 12. Finché la lista era di stringhe secche, `apt-get install` falliva
//! sull'**intero gruppo** al primo nome ignoto e l'installazione si fermava —
//! confermato in campo dal job `container` di R5, dove l'installer non partiva
//! su Debian.
//!
//! Perciò la lista non è di nomi ma di [`PackageSpec`]: un gruppo di
//! **alternative in ordine di preferenza**. Lo `snapshot` risolve ogni gruppo a
//! un nome concreto interrogando apt
//! ([`SystemOps::apt_has_real_candidate`], con il ripiego di
//! [`SystemOps::apt_can_install`] per i nomi virtuali), e da lì in poi tutto il
//! resto della macchina — install, delta, purge, persistenza — lavora su nomi
//! già risolti e non sa nemmeno che esistessero alternative.
//!
//! Le tre regole della risoluzione, in quest'ordine (il perché sta sul metodo
//! `resolve`):
//! 1. se una delle alternative è **già installata**, vince quella. Un cliente
//!    che ha `libtiff-dev` non si vede installare anche `libtiff5-dev`, e il
//!    delta resta onesto (niente da purgare: non l'abbiamo messo noi).
//! 2. altrimenti vince la prima con un candidato **reale**.
//! 3. altrimenti la prima che apt sa installare comunque, cioè un nome
//!    **virtuale** — ripiego, perché un nome virtuale non è purgabile
//!    (A5.1-bis).
//!
//! Se nessuna alternativa è disponibile lo step **fallisce nello snapshot**,
//! prima di mutare, dicendo quale gruppo è vuoto. È l'opposto di degradare in
//! silenzio: un `-dev` che manca diventerebbe un errore di compilazione dentro
//! `pip install`, molto più difficile da ricondurre alla causa.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::context::Context;
use crate::error::StepError;
use crate::step::{decode_snapshot, Step};
use crate::system_ops::{RealSystemOps, SystemOps};

/// Prerequisiti bootstrap: utility comuni a basso rischio.
///
/// Ogni voce è un gruppo di alternative in ordine di preferenza (vedi
/// [`PackageSpec`]); questi quattro nomi sono stabili su tutte le release
/// supportate, quindi non hanno fallback.
pub const BOOTSTRAP_PACKAGES: &[&[&str]] = &[&["git"], &["curl"], &["wget"], &["gettext-base"]];

/// Dipendenze di sistema di Odoo (lista canonica, da `system.sh`).
///
/// Ogni voce è un gruppo di alternative in ordine di preferenza: il primo nome
/// installabile vince. I gruppi con più di un nome sono quelli che cambiano tra
/// release — le divergenze osservate in campo su Debian 11/12 (A5.1).
pub const ODOO_DEPENDENCIES: &[&[&str]] = &[
    &["git"],
    &["curl"],
    &["wget"],
    &["python3-pip"],
    &["python3-dev"],
    &["python3-venv"],
    &["python3-wheel"],
    &["python3-setuptools"],
    &["build-essential"],
    &["gettext-base"],
    // Su Ubuntu 24.04 `libfreetype6-dev` è diventato un nome puramente virtuale
    // (`Provides` di `libfreetype-dev`): installabile ma non purgabile. Il nome
    // reale come alternativa fa sì che il delta contenga qualcosa che l'undo
    // possa davvero rimuovere (A5.1-bis).
    &["libfreetype6-dev", "libfreetype-dev"],
    &["libxml2-dev"],
    &["libzip-dev"],
    &["libldap2-dev"],
    &["libsasl2-dev"],
    &["libjpeg-dev"],
    &["zlib1g-dev"],
    &["libpq-dev"],
    &["libxslt1-dev"],
    // Rinominato senza il soname: Ubuntu 22.04 ha entrambi, Debian 12 solo il
    // secondo.
    &["libtiff5-dev", "libtiff-dev"],
    // Su Ubuntu è un pacchetto di transizione verso `libjpeg-turbo8-dev`; su
    // Debian 12 non esiste e la copertura la dà `libjpeg-dev`, già in lista.
    &["libjpeg8-dev", "libjpeg-turbo8-dev", "libjpeg-dev"],
    &["libopenjp2-7-dev"],
    &["liblcms2-dev"],
    &["libwebp-dev"],
    &["libharfbuzz-dev"],
    &["libfribidi-dev"],
    &["libxcb1-dev"],
    &["libev-dev"],
    &["libc-ares-dev"],
];

/// Dipendenze **opzionali**: utili ma non essenziali all'avvio di Odoo.
///
/// Se nessuna alternativa del gruppo è installabile su questa release, lo step
/// lo dice con un `warn!` e prosegue, invece di fermare l'installazione.
///
/// Qui c'è `node-less`, il compilatore degli asset `.less`. Odoo moderno usa
/// SCSS (compilato in-process da libsass) e parte senza `lessc`; il pacchetto è
/// però stato rimosso da alcune release Debian, e una dipendenza da "nice to
/// have" non deve trasformarsi in un'installazione impossibile. La distinzione
/// esiste **solo** per questo caso: tutto ciò che serve davvero sta nella lista
/// obbligatoria, dove un nome mancante è un errore.
pub const ODOO_OPTIONAL_DEPENDENCIES: &[&[&str]] = &[&["node-less"]];

/// Un requisito di pacchetto: uno o più nomi **alternativi**, in ordine di
/// preferenza, che soddisfano lo stesso bisogno.
///
/// Possiede le `String` invece di prendere `&'static str` perché i test
/// costruiscono gruppi a runtime; le liste di produzione restano `const` (vedi
/// [`ODOO_DEPENDENCIES`]) e passano da [`PackageSpec::group`].
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
    /// warning e non un errore (vedi [`ODOO_OPTIONAL_DEPENDENCIES`]).
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

/// Le specs complete delle dipendenze Odoo: obbligatorie + opzionali.
pub fn odoo_dependency_specs() -> Vec<PackageSpec> {
    let mut all = specs(ODOO_DEPENDENCIES);
    all.extend(
        ODOO_OPTIONAL_DEPENDENCIES
            .iter()
            .map(|g| PackageSpec::optional(g)),
    );
    all
}

/// Esito della risoluzione di un gruppo di alternative su questo sistema.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedPackage {
    /// Una delle alternative è già installata: non entra nel delta.
    AlreadyInstalled(String),
    /// Nessuna installata, ma questa ha un candidato **reale**: entra nel delta.
    Installable(String),
    /// Nessuna installata e nessun candidato reale, ma apt sa installare questo
    /// nome perché è **virtuale** (esiste solo come `Provides` di un altro
    /// pacchetto). Entra nel delta, con una riserva — vedi
    /// [`AptPackagesStep::resolve`].
    Virtual(String),
}

/// Costruisce l'errore dei gruppi senza alcuna alternativa installabile.
///
/// Il messaggio dipende da **quanto sappiamo**, non da quanti gruppi sono
/// caduti. Se l'indice apt non è interrogabile non abbiamo alcuna prova di
/// assenza, e dire "questo pacchetto non esiste su questa release" sarebbe una
/// diagnosi inventata: è il falso positivo A5.1-bis, che in campo ha mandato a
/// cercare la rinomina di un pacchetto che stava benissimo al suo posto.
fn unavailable_packages_error(unavailable: &[PackageSpec], index_populated: bool) -> StepError {
    let groups: Vec<String> = unavailable
        .iter()
        .map(|spec| format!("[{}]", spec.alternatives().join(" | ")))
        .collect();
    let cause = if index_populated {
        "I nomi elencati non esistono su questa release: aggiungi il nome corretto come \
         alternativa in ODOO_DEPENDENCIES (A5.1)"
    } else {
        "L'indice apt non è interrogabile (liste vuote o illeggibili), quindi NON è detto che i \
         pacchetti manchino davvero: esegui 'apt-get update' e riprova. Se l'update non produce \
         un indice valido, il problema è la rete o sources.list"
    };
    StepError::Precondition(format!(
        "nessun pacchetto installabile per {} {}. {cause}",
        if unavailable.len() == 1 {
            "il gruppo"
        } else {
            "i gruppi"
        },
        groups.join(", "),
    ))
}

/// Politica di undo per l'insieme di pacchetti.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoPolicy {
    /// Purga sempre il delta, e solo il delta (delta pesante di Odoo).
    PurgeDelta,
    /// Non purga di default; purga il delta solo con `--aggressive-rollback`
    /// (utility bootstrap comuni).
    KeepUnlessAggressive,
}

/// Snapshot serializzabile del pattern delta (invariante 4).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AptDeltaSnapshot {
    /// Pacchetti già presenti prima di noi: **da non toccare mai** nell'undo.
    pub already_installed: Vec<String>,
    /// Pacchetti che NON c'erano: ciò che installiamo e possiamo rimuovere.
    pub delta: Vec<String>,
}

/// Step di installazione pacchetti apt con pattern delta.
pub struct AptPackagesStep {
    ops: Box<dyn SystemOps>,
    name: &'static str,
    specs: Vec<PackageSpec>,
    policy: UndoPolicy,
    snap: AptDeltaSnapshot,
    /// Nomi risolti dallo `snapshot`, nell'ordine delle specs: è ciò che `run`
    /// passa ad apt. Vive solo in memoria — un rollback da disco non chiama
    /// `run`, e il purge gli guarda il delta persistito.
    resolved: Vec<String>,
    /// Se `true`, il `run` fa `apt-get update` prima di installare. Acceso solo
    /// per `bootstrap-prerequisites`, il primo step apt della sequenza: da lì
    /// l'indice fresco vale per tutti gli step a valle, e ripeterlo sarebbe solo
    /// tempo perso.
    refresh_index: bool,
}

impl AptPackagesStep {
    /// Prerequisiti bootstrap (undo non purga senza `--aggressive-rollback`).
    pub fn bootstrap() -> Self {
        Self::bootstrap_with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn bootstrap_with_ops(ops: Box<dyn SystemOps>) -> Self {
        let mut step = Self::with_specs(
            ops,
            "bootstrap-prerequisites",
            specs(BOOTSTRAP_PACKAGES),
            UndoPolicy::KeepUnlessAggressive,
        );
        // È il primo step apt: è qui che l'indice va aggiornato, per tutti.
        step.refresh_index = true;
        step
    }

    /// Dipendenze di sistema di Odoo (undo purga il delta, e solo il delta).
    pub fn odoo_dependencies() -> Self {
        Self::odoo_dependencies_with_ops(Box::new(RealSystemOps::new()))
    }

    pub fn odoo_dependencies_with_ops(ops: Box<dyn SystemOps>) -> Self {
        Self::with_specs(
            ops,
            "install-system-dependencies",
            odoo_dependency_specs(),
            UndoPolicy::PurgeDelta,
        )
    }

    /// Costruttore generico su nomi secchi, uno per gruppo (usato dai test con
    /// liste ad hoc, dove le alternative non c'entrano).
    pub fn custom(
        ops: Box<dyn SystemOps>,
        name: &'static str,
        packages: Vec<String>,
        policy: UndoPolicy,
    ) -> Self {
        let specs = packages.iter().map(|p| PackageSpec::one(p)).collect();
        Self::with_specs(ops, name, specs, policy)
    }

    /// Costruttore generico su gruppi di alternative.
    pub fn with_specs(
        ops: Box<dyn SystemOps>,
        name: &'static str,
        specs: Vec<PackageSpec>,
        policy: UndoPolicy,
    ) -> Self {
        Self {
            ops,
            name,
            specs,
            policy,
            snap: AptDeltaSnapshot::default(),
            resolved: Vec::new(),
            refresh_index: false,
        }
    }

    /// Sceglie, dentro un gruppo, il nome da usare su **questo** sistema.
    ///
    /// Tre domande, in quest'ordine preciso.
    ///
    /// 1. **"Ne hai già uno?"** Se sì vince quello. Chiederlo per primo evita di
    ///    installare `libtiff5-dev` a un cliente che ha già `libtiff-dev`,
    ///    gonfiando il delta con un pacchetto che il rollback poi purgherebbe —
    ///    corretto ma inutile, e sul sistema di qualcun altro l'inutile è un
    ///    costo.
    /// 2. **"Quale ha un candidato reale?"** La via veloce (`apt-cache policy`),
    ///    che copre tutti i casi normali.
    /// 3. **"Quale sapresti installare comunque?"** La via lenta
    ///    (`apt-get install -s`), che copre i nomi **virtuali**.
    ///
    /// # Perché un nome reale batte un nome virtuale (A5.1-bis)
    ///
    /// Un nome puramente virtuale è installabile ma **non è purgabile**, e
    /// questo rompe il pattern delta in silenzio. Su Ubuntu 24.04
    /// `libfreetype6-dev` esiste solo come `Provides` di `libfreetype-dev`:
    /// `apt-get install libfreetype6-dev` funziona, ma dopo
    /// `dpkg-query` non conosce quel nome (`not-installed`) e
    /// `apt-get purge libfreetype6-dev` esce **0 rimuovendo zero pacchetti**.
    /// Il delta conterrebbe un nome che l'undo non può reclamare: il rollback
    /// direbbe di aver purgato e `libfreetype-dev` resterebbe installato. Un
    /// residuo invisibile, cioè la cosa peggiore.
    ///
    /// Perciò il livello 3 è un **ripiego**, non una scorciatoia: si prende un
    /// nome virtuale solo se nessuna alternativa del gruppo ne ha uno reale, e
    /// lo si dice nei log.
    fn resolve(&self, spec: &PackageSpec) -> Option<ResolvedPackage> {
        if let Some(installed) = spec
            .alternatives()
            .iter()
            .find(|name| self.ops.dpkg_is_installed(name))
        {
            return Some(ResolvedPackage::AlreadyInstalled(installed.clone()));
        }
        if let Some(real) = spec
            .alternatives()
            .iter()
            .find(|name| self.ops.apt_has_real_candidate(name))
        {
            return Some(ResolvedPackage::Installable(real.clone()));
        }
        spec.alternatives()
            .iter()
            .find(|name| self.ops.apt_can_install(name))
            .map(|name| ResolvedPackage::Virtual(name.clone()))
    }

    /// Aggiorna l'indice apt prima di installare (A5.1-bis).
    ///
    /// Vive nel `run` di `bootstrap-prerequisites`, che è il primo step apt della
    /// sequenza: quando `install-system-dependencies` fa il proprio `snapshot` e
    /// interroga i candidati, l'indice è già fresco. Metterlo nello `snapshot`
    /// sarebbe più comodo e sarebbe **sbagliato**: uno snapshot non muta, mai
    /// (C4). `apt-get update` scrive in `/var/lib/apt/lists`.
    ///
    /// # Tolleranza ai repository irraggiungibili
    ///
    /// `apt-get update` esce non-zero anche quando **un solo** repository di
    /// terze parti non risponde, mentre gli indici ufficiali sono stati
    /// scaricati benissimo. Bloccare lì significherebbe rendere l'installer
    /// ostaggio di un PPA rotto che non ci riguarda. Quindi: se l'update
    /// fallisce ma l'indice risulta comunque popolato, si prosegue con un
    /// `warn!`; si fallisce solo se dopo il tentativo non c'è **nessun** indice
    /// da interrogare, che è la condizione in cui gli step successivi non
    /// potrebbero decidere nulla.
    fn refresh_apt_index(&self, ctx: &Context) -> Result<(), StepError> {
        if ctx.dry_run {
            info!(step = self.name, "run (dry-run): apt-get update");
            return Ok(());
        }
        let Err(e) = self.ops.apt_update() else {
            info!(step = self.name, "run: indice apt aggiornato");
            return Ok(());
        };
        if self.ops.apt_index_is_populated() {
            warn!(
                step = self.name,
                error = %e,
                "run: apt-get update ha segnalato errori (repository irraggiungibile?), \
                 ma l'indice apt è popolato: proseguo"
            );
            return Ok(());
        }
        Err(StepError::Precondition(format!(
            "apt-get update è fallito e l'indice apt resta vuoto: senza indice non è possibile \
             stabilire quali pacchetti siano installabili. Verifica rete e sources.list. \
             Errore originale: {e}"
        )))
    }

    /// Purga il delta persistito (best-effort). Usa il delta dello snapshot,
    /// **non** ricalcolato dallo stato corrente (che nel frattempo è cambiato:
    /// il run ha installato i pacchetti).
    ///
    /// # Niente `apt-get autoremove` (A3.2)
    ///
    /// Il rollback **non** lancia un `autoremove` globale. `autoremove` agisce
    /// su tutto il sistema: rimuove qualunque pacchetto auto-installato che apt
    /// consideri orfano *in quel momento*, anche del tutto estraneo a Odoo e
    /// tirato dentro da altro software. Sarebbe una rimozione non delimitata dal
    /// nostro delta, cioè l'esatto contrario del principio chirurgico che regge
    /// tutto il rollback. Il purge del delta è già mirato: le dipendenze tirate
    /// dentro dai *nostri* pacchetti restano installate, il che è rumore innocuo
    /// a fronte del rischio di disinstallare roba altrui.
    ///
    /// Il purge passa da [`purge_with_dpkg_recovery`](crate::steps::purge_with_dpkg_recovery),
    /// che rimette in sesto `dpkg` se uno step a valle l'ha lasciato rotto:
    /// altrimenti apt rifiuta di operare e il delta resta installato (A-RT-2).
    fn purge_delta(&self, ctx: &Context) -> Result<(), StepError> {
        if self.snap.delta.is_empty() {
            info!(step = self.name, "undo: delta vuoto, niente da purgare");
            return Ok(());
        }
        if ctx.dry_run {
            info!(step = self.name, delta = ?self.snap.delta, "undo (dry-run): purgerei il delta");
            return Ok(());
        }
        let refs: Vec<&str> = self.snap.delta.iter().map(String::as_str).collect();
        crate::steps::purge_with_dpkg_recovery(self.ops.as_ref(), self.name, &refs);
        Ok(())
    }
}

impl Step for AptPackagesStep {
    fn name(&self) -> &str {
        self.name
    }

    /// Risolve i gruppi di alternative **e** calcola il delta: due operazioni
    /// che devono avvenire nello stesso istante, prima di ogni mutazione. Il
    /// delta è espresso in nomi già risolti, quindi il purge dell'undo non ha
    /// bisogno di sapere che esistessero alternative.
    fn snapshot(&mut self, _ctx: &Context) -> Result<(), StepError> {
        let mut already_installed = Vec::new();
        let mut delta = Vec::new();
        let mut resolved = Vec::new();
        let mut unavailable = Vec::new();

        for spec in &self.specs {
            match self.resolve(spec) {
                Some(ResolvedPackage::AlreadyInstalled(name)) => {
                    resolved.push(name.clone());
                    already_installed.push(name);
                }
                Some(ResolvedPackage::Installable(name)) => {
                    resolved.push(name.clone());
                    delta.push(name);
                }
                Some(ResolvedPackage::Virtual(name)) => {
                    warn!(
                        step = self.name,
                        pacchetto = %name,
                        gruppo = ?spec.alternatives(),
                        "snapshot: nessun candidato reale nel gruppo, uso un nome VIRTUALE. \
                         È installabile, ma il purge dell'undo potrebbe non reclamarlo: \
                         considera di aggiungere il nome reale come alternativa"
                    );
                    resolved.push(name.clone());
                    delta.push(name);
                }
                None if spec.is_required() => unavailable.push(spec.clone()),
                None => warn!(
                    step = self.name,
                    gruppo = ?spec.alternatives(),
                    "snapshot: nessuna alternativa disponibile per un gruppo OPZIONALE, proseguo senza"
                ),
            }
        }

        // Prima di dichiarare assente un gruppo: sappiamo abbastanza per dirlo?
        // Con un indice apt non interrogabile la risposta è no — ogni nome
        // risulta "non disponibile" e il verdetto sarebbe cecità travestita da
        // diagnosi. È il falso positivo A5.1-bis.
        //
        // Chi è questo step decide cosa fare di quella cecità:
        // - `bootstrap-prerequisites` (`refresh_index`) è lo step che **sistemerà
        //   l'indice lui stesso**, nel proprio `run`, e il suo snapshot gira per
        //   forza prima. Fermarlo qui renderebbe impossibile installare su una
        //   macchina appena creata — cioè proprio il caso che l'update esiste per
        //   risolvere. Si prosegue coi nomi preferiti (che nella lista bootstrap
        //   non hanno alternative: non c'è nulla da scegliere) e si lascia
        //   parlare apt nel `run`.
        // - ogni altro step gira **dopo** l'update: se lì l'indice è ancora
        //   inservibile è un problema vero, e va detto con il messaggio giusto.
        if !unavailable.is_empty() {
            let index_populated = self.ops.apt_index_is_populated();
            if index_populated || !self.refresh_index {
                return Err(unavailable_packages_error(&unavailable, index_populated));
            }
            for spec in unavailable.drain(..) {
                if spec.is_required() {
                    warn!(
                        step = self.name,
                        gruppo = ?spec.alternatives(),
                        "snapshot: indice apt non interrogabile, non posso verificare questo gruppo. \
                         Uso il nome preferito e lascio decidere ad apt nel run (dopo apt-get update)"
                    );
                    let preferred = spec.preferred().to_string();
                    resolved.push(preferred.clone());
                    delta.push(preferred);
                } else {
                    // Un opzionale non verificabile si salta: aggiungerlo alla
                    // riga di apt farebbe fallire l'install dell'INTERO gruppo
                    // se poi non esistesse, che è il contrario di "opzionale".
                    warn!(
                        step = self.name,
                        gruppo = ?spec.alternatives(),
                        "snapshot: indice apt non interrogabile e gruppo OPZIONALE, proseguo senza"
                    );
                }
            }
        }

        info!(
            step = self.name,
            already = already_installed.len(),
            delta = delta.len(),
            risolti = resolved.len(),
            "snapshot delta apt"
        );
        // I NOMI, non solo il conteggio: il log è il **diario** dell'esecuzione,
        // il manifesto è lo **stato**. Sono due cose diverse, e confonderle è
        // costato A-R8-1. Dopo un rollback il manifesto — correttamente — non
        // elenca più nulla, ma «quali pacchetti abbiamo aggiunto» resta una
        // domanda legittima: per il post-mortem di un cliente e per le
        // asserzioni di pulizia della CI.
        info!(
            step = self.name,
            pacchetti = %delta.join(" "),
            "delta apt: pacchetti aggiunti da noi"
        );
        info!(
            step = self.name,
            pacchetti = %already_installed.join(" "),
            "delta apt: pacchetti già presenti, mai toccati"
        );
        self.resolved = resolved;
        self.snap = AptDeltaSnapshot {
            already_installed,
            delta,
        };
        Ok(())
    }

    fn run(&mut self, ctx: &Context) -> Result<(), StepError> {
        // PRIMA di ogni uscita anticipata: l'indice apt serve agli step a valle,
        // non a noi. Su un runner GitHub le utility bootstrap sono già installate
        // → delta vuoto → con il `return` prima dell'update,
        // `install-system-dependencies` interrogherebbe un indice stantìo e
        // boccerebbe pacchetti che esistono (A5.1-bis, il bug di campo).
        if self.refresh_index {
            self.refresh_apt_index(ctx)?;
        }

        if self.snap.delta.is_empty() {
            info!(
                step = self.name,
                "run: tutti i pacchetti già presenti, niente da installare"
            );
            return Ok(());
        }
        if ctx.dry_run {
            info!(
                step = self.name,
                "run (dry-run): apt-get install dell'intera lista (installa solo i mancanti)"
            );
            return Ok(());
        }
        // Installa l'intera lista risolta: apt aggiunge solo i mancanti
        // (idempotente).
        let refs: Vec<&str> = self.resolved.iter().map(String::as_str).collect();
        self.ops.apt_install(&refs)?;
        info!(
            step = self.name,
            installed = self.snap.delta.len(),
            "run: pacchetti installati"
        );
        Ok(())
    }

    fn undo(&self, ctx: &Context) -> Result<(), StepError> {
        match self.policy {
            UndoPolicy::PurgeDelta => self.purge_delta(ctx),
            UndoPolicy::KeepUnlessAggressive => {
                if ctx.aggressive_rollback {
                    self.purge_delta(ctx)
                } else {
                    info!(
                        step = self.name,
                        "undo NO-OP: le utility comuni (git/curl/wget/gettext) restano installate. \
                         Usa --aggressive-rollback per purgare anche queste."
                    );
                    Ok(())
                }
            }
        }
    }

    fn snapshot_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.snap).unwrap_or(serde_json::Value::Null)
    }

    /// Reidrata il **delta**: i pacchetti che non c'erano prima di noi.
    ///
    /// Ricalcolarlo sarebbe sbagliato per costruzione — dopo il `run` tutta la
    /// lista risulta installata e il delta apparirebbe vuoto (nessun purge) o,
    /// peggio, coinciderebbe con l'intera lista, portando l'undo a purgare
    /// pacchetti che il cliente aveva già.
    fn rehydrate(&mut self, snapshot: &serde_json::Value) -> Result<(), StepError> {
        let snap = decode_snapshot(self.name(), snapshot)?;
        self.snap = snap;
        Ok(())
    }
}
