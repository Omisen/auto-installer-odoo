//! Test di M11: **quale interprete** crea il virtualenv, e cosa ne consegue.
//!
//! Il difetto che questa fase chiude (`A-MD-7`) non è nel nostro codice ma nei
//! pin di Odoo: su Python 3.14 — il sistema di Fedora ≥ 43 — Odoo 18 pinna
//! `gevent==24.11.1`, che per quell'interprete non ha una wheel e il cui C
//! generato non compila. M10 lo *dice*; M11 fa in modo che non succeda,
//! facendo nascere il venv su un interprete che quei pin coprono.
//!
//! Verificato in campo prima di scrivere una riga (VM Fedora 44): con
//! `python3.13` l'intero `requirements.txt` si installa, `gevent` incluso e
//! come wheel già compilata.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::checks::{choose_python, PythonPlan, NEWEST_TESTED_PYTHON};
use invok::context::Context;
use invok::packaging::{AlternatePython, PackageSpec};
use invok::step::Step;
use invok::steps::apt_packages::AptPackagesStep;
use invok::steps::create_virtualenv::CreateVirtualenv;
use std::path::PathBuf;

fn fedora_alternates() -> Vec<AlternatePython> {
    vec![
        AlternatePython::new((3, 13), "python3.13", "python3.13-devel"),
        AlternatePython::new((3, 12), "python3.12", "python3.12-devel"),
    ]
}

fn system_dev() -> Vec<String> {
    vec!["python3-devel".to_string()]
}

// --- La scelta ---------------------------------------------------------------

/// Un Python coperto dai pin non si tocca: nessun interprete in più, nessun
/// pacchetto nel delta.
///
/// È la metà che rende M11 **invisibile** dove non serve — Debian, Ubuntu, e
/// ogni Fedora fino alla 42. Una fase che cambiasse comportamento anche lì
/// sarebbe una fase molto più rischiosa di quella che serviva.
#[test]
fn a_supported_system_interpreter_is_left_alone() {
    let plan = choose_python(Some((3, 12)), &fedora_alternates(), &system_dev());
    assert_eq!(plan, PythonPlan::default());
    assert!(plan.is_system());
    assert_eq!(plan.command, "python3");

    // Anche esattamente sulla soglia: «provato» vuol dire che lì si arriva in
    // fondo, quindi non c'è niente da sostituire.
    let plan = choose_python(
        Some(NEWEST_TESTED_PYTHON),
        &fedora_alternates(),
        &system_dev(),
    );
    assert!(
        plan.is_system(),
        "sulla versione provata non si cambia nulla"
    );
}

/// Un Python più recente dei pin fa nascere il venv sull'interprete più recente
/// **fra quelli coperti**, non sul più vecchio disponibile.
///
/// La direzione conta: un interprete più vicino a quello di sistema riceve
/// aggiornamenti di sicurezza più a lungo, e resta comunque dentro ciò che
/// l'installer prova davvero.
#[test]
fn an_unsupported_system_interpreter_is_replaced_by_the_newest_covered_one() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());

    assert_eq!(plan.command, "python3.13", "3.13 batte 3.12: è più recente");
    assert!(!plan.is_system());
    assert_eq!(
        plan.packages,
        vec!["python3.13".to_string(), "python3.13-devel".to_string()],
        "l'interprete da solo non basta: senza header non si compila nessuna estensione"
    );
    assert_eq!(
        plan.supersedes,
        system_dev(),
        "gli header del Python di sistema non servono più a nessuno"
    );
}

/// Se le uniche alternative sono a loro volta più recenti dei pin, non si
/// installa niente: si resta sul sistema, e a parlare è l'avviso di M10.
///
/// È il ramo che impedisce alla scelta di diventare «prendi comunque qualcosa»:
/// installare un secondo interprete altrettanto scoperto sarebbe una mutazione
/// su una macchina cliente in cambio di nulla.
#[test]
fn an_alternate_that_is_just_as_new_is_not_a_solution() {
    let troppo_nuovi = vec![AlternatePython::new(
        (3, 15),
        "python3.15",
        "python3.15-devel",
    )];
    let plan = choose_python(Some((3, 14)), &troppo_nuovi, &system_dev());
    assert!(
        plan.is_system(),
        "nessun interprete coperto: si resta dove si è"
    );
    assert!(plan.packages.is_empty());
}

/// Nessuna alternativa impacchettata (Debian, Ubuntu) → si resta sul sistema.
#[test]
fn without_alternates_there_is_nothing_to_choose() {
    let plan = choose_python(Some((3, 14)), &[], &system_dev());
    assert!(plan.is_system());
}

/// «Non so che Python ci sia» non è «è troppo nuovo».
///
/// Da un'informazione assente non si conclude niente, e men che meno si
/// installa un secondo interprete sulla macchina di un cliente.
#[test]
fn an_unknown_system_interpreter_does_not_trigger_an_installation() {
    let plan = choose_python(None, &fedora_alternates(), &system_dev());
    assert!(plan.is_system());
    assert!(plan.packages.is_empty());
}

// --- Le conseguenze sulla lista dei pacchetti --------------------------------

fn specs(names: &[&str]) -> Vec<PackageSpec> {
    names.iter().map(|n| PackageSpec::one(n)).collect()
}

/// Con l'interprete di sistema la lista dei pacchetti resta **identica**.
#[test]
fn the_package_list_is_untouched_when_the_system_interpreter_is_used() {
    let lista = specs(&["python3-devel", "gcc", "libpq-devel"]);
    assert_eq!(PythonPlan::default().adapt_specs(&lista), lista);
}

/// Con un interprete alternativo: fuori gli header di sistema, dentro i suoi.
///
/// Il resto della lista non si tocca — `gcc` e `libpq-devel` servono comunque,
/// e le estensioni C che pip compila sono le stesse.
#[test]
fn the_alternate_interpreter_replaces_the_system_headers_and_nothing_else() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let adattata = plan.adapt_specs(&specs(&["python3-devel", "gcc", "libpq-devel"]));
    let nomi: Vec<&str> = adattata.iter().map(|s| s.preferred()).collect();

    assert!(
        !nomi.contains(&"python3-devel"),
        "gli header del Python di sistema non servono a un venv su 3.13: {nomi:?}"
    );
    assert!(nomi.contains(&"python3.13"), "manca l'interprete: {nomi:?}");
    assert!(
        nomi.contains(&"python3.13-devel"),
        "mancano i suoi header, e sei estensioni C non compilerebbero: {nomi:?}"
    );
    assert!(
        nomi.contains(&"gcc") && nomi.contains(&"libpq-devel"),
        "il resto della lista non c'entra e deve restare: {nomi:?}"
    );
}

// --- Le conseguenze sugli step -----------------------------------------------

fn ctx_with(plan: PythonPlan) -> Context {
    Context {
        odoo_user: "odoo".to_string(),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        python: plan,
        ..Default::default()
    }
}

fn installed(ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter_map(|o| match o {
            Op::PkgInstall(pkgs) => Some(pkgs.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// L'interprete lo installa `install-system-dependencies`, il cui undo **purga
/// il delta**.
///
/// Non `bootstrap-prerequisites`: lì l'undo lascia installato ciò che ha
/// aggiunto (git, curl, le utility comuni), e un interprete da 43 MB messo da
/// noi e mai rimosso sarebbe un residuo dentro il perimetro che il rollback
/// promette di riportare com'era.
#[test]
fn the_interpreter_is_installed_by_the_step_whose_undo_purges_it() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let ctx = ctx_with(plan);

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut deps = AptPackagesStep::odoo_dependencies_with_ops(Box::new(mock));
    deps.snapshot(&ctx).expect("snapshot");
    deps.run(&ctx).expect("run");
    let pacchetti = installed(&ops_of(&log));
    assert!(
        pacchetti.iter().any(|p| p == "python3.13"),
        "install-system-dependencies deve portare l'interprete: {pacchetti:?}"
    );

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut boot = AptPackagesStep::bootstrap_with_ops(Box::new(mock));
    boot.snapshot(&ctx).expect("snapshot");
    boot.run(&ctx).expect("run");
    let pacchetti = installed(&ops_of(&log));
    assert!(
        !pacchetti.iter().any(|p| p.starts_with("python3.1")),
        "bootstrap NON deve portarlo: il suo undo non lo rimuoverebbe ({pacchetti:?})"
    );
}

/// Il venv nasce sull'interprete scelto, e la precondizione interroga **quello**.
///
/// Sono la stessa domanda posta due volte, e devono avere la stessa risposta:
/// chiedere di `ensurepip` a `python3` e poi creare il venv con `python3.13`
/// sarebbe di nuovo un controllo che parla di un'altra cosa (A-R6-1).
#[test]
fn the_virtualenv_is_born_on_the_chosen_interpreter() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let ctx = ctx_with(plan);

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let mut step = CreateVirtualenv::with_ops(Box::new(mock));
    step.snapshot(&ctx).expect("snapshot");
    step.run(&ctx).expect("run");

    assert!(
        ops_of(&log).iter().any(|o| matches!(
            o,
            Op::CreateVenv { python, .. } if python == "python3.13"
        )),
        "il venv deve nascere su python3.13: {:?}",
        ops_of(&log)
    );
}

/// La diagnosi di A-MD-7 interroga l'interprete **del venv**, non quello di
/// sistema.
///
/// Dopo M11 i due possono divergere, ed è il caso normale su Fedora ≥ 43: se il
/// venv gira su 3.13 e qualcosa non compila, dire «questo sistema usa Python
/// 3.14» manderebbe a cercare una causa che non c'è. È la stessa forma del
/// difetto che M11 corregge, un livello più in là — e senza registrare *quale*
/// nome viene interrogato nessun test potrebbe vederla.
#[test]
fn the_failure_diagnosis_asks_the_interpreter_the_venv_actually_uses() {
    let plan = choose_python(Some((3, 14)), &fedora_alternates(), &system_dev());
    let mut ctx = ctx_with(plan);
    ctx.dry_run = false;

    let cfg = MockConfig {
        requirements_content: Some(
            "gevent==24.11.1 ; sys_platform != 'win32' and python_version >= '3.13'\n".to_string(),
        ),
        run_as_user_fails_on: Some("requirements-gevent".to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let mut step = invok::steps::install_python_requirements::InstallPythonRequirements::with_ops(
        Box::new(mock),
    );
    step.snapshot(&ctx).expect("snapshot");
    let _ = step.run(&ctx).expect_err("il passo gevent è fallito");

    assert!(
        ops_of(&log)
            .iter()
            .any(|o| matches!(o, Op::PythonVersion(nome) if nome == "python3.13")),
        "la diagnosi deve chiedere la versione a python3.13, non a python3: {:?}",
        ops_of(&log)
    );
}

/// Fedora deve offrire **almeno un** interprete coperto dai pin, o M11 su
/// Fedora non fa niente in silenzio.
///
/// È la domanda di rito applicata a una tabella invece che a un controllo: in
/// produzione, su una Fedora 44, questa lista può portare a una scelta diversa
/// da «resta sul sistema»? Se un domani `NEWEST_TESTED_PYTHON` salisse o la
/// lista si svuotasse, il codice continuerebbe a funzionare e non
/// installerebbe più nulla — e nessun rosso lo direbbe.
#[test]
fn fedora_offers_at_least_one_interpreter_covered_by_the_pins() {
    use invok::checks::python_is_newer_than_tested;
    use invok::packaging::dnf::DnfBackend;
    use invok::packaging::PackageManager;

    let catalog = DnfBackend.catalog();
    assert!(
        !catalog.alternate_pythons.is_empty(),
        "senza alternative M11 su Fedora è codice morto"
    );
    assert!(
        catalog
            .alternate_pythons
            .iter()
            .any(|alt| !python_is_newer_than_tested(alt.version)),
        "nessuna delle alternative è coperta dai pin: la scelta ripiegherebbe \
         sempre sull'interprete di sistema"
    );
    // E ogni alternativa porta i suoi header: l'interprete da solo non compila.
    for alt in &catalog.alternate_pythons {
        assert!(
            alt.devel.starts_with(&alt.interpreter),
            "{} non porta gli header corrispondenti ({})",
            alt.interpreter,
            alt.devel
        );
    }
}
