#!/usr/bin/env bash
# =============================================================================
# lib/checks.sh — Prerequisiti, OS detection, root check
# =============================================================================
# Funzioni esposte:
#   check_root      → verifica che lo script giri come root
#   check_os        → verifica OS supportato (Ubuntu/Debian) e versione minima
#   check_ports     → verifica che le porte richieste siano libere
#   check_disk      → verifica spazio disco sufficiente
#   check_commands  → verifica presenza dei comandi di sistema richiesti
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# check_root
#   Verifica che lo script sia eseguito come root (UID 0).
#   Esce con codice 1 in caso contrario.
# ---------------------------------------------------------------------------
check_root() {
    log "Verifica privilegi root..."

    if [[ "${EUID}" -ne 0 ]]; then
        error "Questo script deve essere eseguito come root. Riprova con: sudo ${0}"
    fi

    log "✔ Esecuzione come root confermata."
}

# ---------------------------------------------------------------------------
# check_sudo_user
#   Verifica che lo script sia eseguito via sudo da un utente normale,
#   non direttamente come root (es: sudo -i, su -).
#   Esce con codice 1 se SUDO_USER non è valorizzato.
# ---------------------------------------------------------------------------
check_sudo_user() {
    log "Verifica esecuzione via sudo da utente normale..."

    if [[ -z "${SUDO_USER}" ]]; then
        error "Questo script deve essere eseguito via sudo da un utente normale."
        error "Usa: sudo $0 [opzioni]"
        error "Non utilizzare: sudo -i, su -, o login diretto come root."
    fi

    log "✔ Esecuzione confermata con sudo (utente: ${SUDO_USER})."
}

# ---------------------------------------------------------------------------
# check_os
#   Verifica che il sistema operativo sia Ubuntu o Debian.
#   Verifica la versione minima supportata:
#     - Ubuntu  >= 22.04
#     - Debian  >= 11
#   Popola le variabili globali:
#     OS_ID       (ubuntu | debian)
#     OS_CODENAME (es. jammy, bookworm)
#     OS_VERSION  (es. 22.04, 12)
# ---------------------------------------------------------------------------
check_os() {
    log "Verifica sistema operativo..."

    local os_release="/etc/os-release"

    if [[ ! -f "${os_release}" ]]; then
        error "File ${os_release} non trovato. OS non riconoscibile."
        exit 1
    fi

    # Carica le variabili dal file os-release in un subshell sicuro
    # e le esporta esplicitamente per evitare collisioni di namespace.
    OS_ID="$(grep -E '^ID=' "${os_release}" | cut -d= -f2 | tr -d '"' | tr '[:upper:]' '[:lower:]')"
    OS_CODENAME="$(grep -E '^VERSION_CODENAME=' "${os_release}" | cut -d= -f2 | tr -d '"' || true)"
    OS_VERSION="$(grep -E '^VERSION_ID=' "${os_release}" | cut -d= -f2 | tr -d '"')"

    export OS_ID OS_CODENAME OS_VERSION

    log "  OS rilevato : ${OS_ID}"
    log "  Versione    : ${OS_VERSION}"
    log "  Codename    : ${OS_CODENAME:-n/a}"

    case "${OS_ID}" in
        ubuntu)
            _check_ubuntu_version "${OS_VERSION}"
            ;;
        debian)
            _check_debian_version "${OS_VERSION}"
            ;;
        *)
            error "Sistema operativo '${OS_ID}' non supportato."
            error "Sistemi supportati: Ubuntu >= 22.04, Debian >= 11."
            exit 1
            ;;
    esac

    log "✔ OS supportato: ${OS_ID} ${OS_VERSION} (${OS_CODENAME:-})."
}

# ---------------------------------------------------------------------------
# _check_ubuntu_version <version_string>
#   Privata — controlla che la versione Ubuntu sia >= 22.04
# ---------------------------------------------------------------------------
_check_ubuntu_version() {
    local version="${1}"
    local major minor

    # VERSION_ID per Ubuntu è nel formato "22.04"
    major="$(echo "${version}" | cut -d. -f1)"
    minor="$(echo "${version}" | cut -d. -f2)"

    if [[ "${major}" -lt 22 ]] || { [[ "${major}" -eq 22 ]] && [[ "${minor}" -lt 4 ]]; }; then
        error "Ubuntu ${version} non supportato."
        error "Versione minima richiesta: Ubuntu 22.04 LTS."
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# _check_debian_version <version_string>
#   Privata — controlla che la versione Debian sia >= 11
# ---------------------------------------------------------------------------
_check_debian_version() {
    local version="${1}"

    # VERSION_ID per Debian è un intero ("11", "12")
    if [[ "${version}" -lt 11 ]]; then
        error "Debian ${version} non supportato."
        error "Versione minima richiesta: Debian 11 (Bullseye)."
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# check_ports
#   Verifica che le porte necessarie non siano già in uso.
#   Usa la variabile globale ODOO_PORT (default 8069).
#   Se WITH_NGINX=true controlla anche la porta 80 e 443.
# ---------------------------------------------------------------------------
check_ports() {
    log "Verifica disponibilità porte..."

    local ports=("${ODOO_PORT:-8069}")

    if [[ "${WITH_NGINX:-false}" == "true" ]]; then
        ports+=(80 443)
    fi

    local failed=0

    for port in "${ports[@]}"; do
        if _port_in_use "${port}"; then
            warn "Porta ${port} già in uso."
            failed=1
        else
            log "  ✔ Porta ${port} disponibile."
        fi
    done

    if [[ "${failed}" -eq 1 ]]; then
        error "Una o più porte richieste sono già occupate."
        error "Libera le porte sopra indicate prima di procedere."
        exit 1
    fi

    log "✔ Tutte le porte necessarie sono disponibili."
}

# ---------------------------------------------------------------------------
# _port_in_use <port>
#   Privata — ritorna 0 (true) se la porta è occupata, 1 altrimenti.
#   Prova ss, poi netstat, poi lsof come fallback.
# ---------------------------------------------------------------------------
_port_in_use() {
    local port="${1}"

    if command -v ss &>/dev/null; then
        ss -lntu 2>/dev/null | grep -qE ":${port}\s" && return 0 || return 1
    elif command -v netstat &>/dev/null; then
        netstat -lntu 2>/dev/null | grep -qE ":${port}\s" && return 0 || return 1
    elif command -v lsof &>/dev/null; then
        lsof -iTCP:"${port}" -sTCP:LISTEN &>/dev/null && return 0 || return 1
    else
        warn "Impossibile verificare la porta ${port}: nessuno strumento disponibile (ss/netstat/lsof)."
        return 1  # non bloccante: assumiamo libera
    fi
}

# ---------------------------------------------------------------------------
# check_disk
#   Verifica che ci sia abbastanza spazio libero sul filesystem di ODOO_HOME.
#   Soglia minima: 5 GB (configurabile via MIN_DISK_GB).
# ---------------------------------------------------------------------------
check_disk() {
    local required_gb="${MIN_DISK_GB:-5}"
    local target_dir="${ODOO_HOME:-/opt/odoo}"

    log "Verifica spazio disco (minimo: ${required_gb} GB su ${target_dir})..."

    # Crea la directory target se non esiste ancora (per poterla misurare)
    mkdir -p "${target_dir}"

    local available_kb
    available_kb="$(df -Pk "${target_dir}" | awk 'NR==2 {print $4}')"
    local available_gb=$(( available_kb / 1024 / 1024 ))

    log "  Spazio disponibile: ${available_gb} GB"

    if [[ "${available_gb}" -lt "${required_gb}" ]]; then
        error "Spazio insufficiente su ${target_dir}."
        error "Disponibile: ${available_gb} GB — Richiesto: ${required_gb} GB."
        exit 1
    fi

    log "✔ Spazio disco sufficiente (${available_gb} GB disponibili)."
}

# ---------------------------------------------------------------------------
# bootstrap_prerequisites
#   Installa i pacchetti minimi indispensabili prima di qualsiasi altro step.
#   Questi tool non possono essere assunti presenti su una VM fresh e vengono
#   richiesti da fasi successive (install_dependencies, config.sh, ecc.).
#
#   Pacchetti installati:
#     git           — clone repo Odoo
#     curl / wget   — download wkhtmltopdf e altri asset
#     gettext-base  — fornisce envsubst, usato da config.sh per i template
#
#   NON include: python3, pip, psql — installati da install_dependencies
#   e setup_postgres nelle fasi successive.
# ---------------------------------------------------------------------------
bootstrap_prerequisites() {
    # Sopprime needrestart anche qui — bootstrap gira prima di export_vars
    export NEEDRESTART_MODE=a
    export DEBIAN_FRONTEND=noninteractive

    local bootstrap_pkgs=(
        git
        curl
        wget
        gettext-base
    )

    local to_install=()
    for pkg in "${bootstrap_pkgs[@]}"; do
        if ! command -v "${pkg}" &>/dev/null 2>&1; then
            to_install+=("${pkg}")
        fi
    done

    # gettext-base non espone un binario omonimo — controlla envsubst
    if ! command -v envsubst &>/dev/null; then
        # aggiungi solo se non già in lista
        [[ " ${to_install[*]} " != *" gettext-base "* ]] && to_install+=(gettext-base)
    fi

    if [[ ${#to_install[@]} -eq 0 ]]; then
        log "✔ Prerequisiti bootstrap già presenti."
        return 0
    fi

    log "Installazione prerequisiti bootstrap: ${to_install[*]}"
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        "${to_install[@]}"
    log "✔ Prerequisiti bootstrap installati."
}

# ---------------------------------------------------------------------------
# check_commands
#   Verifica che i comandi di sistema fondamentali del SO siano presenti.
#   Controlla SOLO ciò che apt-get non può installare autonomamente:
#   systemctl (init system) e apt-get stesso.
#   git/python3/pip3/psql/envsubst vengono installati nelle fasi successive.
# ---------------------------------------------------------------------------
check_commands() {
    log "Verifica comandi di sistema richiesti..."

    # Solo prerequisiti del SO — non installabili dallo script stesso
    local required_cmds=(
        apt-get    # gestore pacchetti — senza questo nulla funziona
        systemctl  # init system — necessario per enable/start servizi
    )

    local optional_cmds=(
        nginx
        certbot
    )

    local missing=0

    for cmd in "${required_cmds[@]}"; do
        if ! command -v "${cmd}" &>/dev/null; then
            warn "Comando mancante (obbligatorio): ${cmd}"
            missing=$(( missing + 1 ))
        else
            log "  ✔ ${cmd}"
        fi
    done

    for cmd in "${optional_cmds[@]}"; do
        if ! command -v "${cmd}" &>/dev/null; then
            log "  ℹ  ${cmd} non trovato (opzionale — verrà installato se necessario)."
        else
            log "  ✔ ${cmd}"
        fi
    done

    if [[ "${missing}" -gt 0 ]]; then
        error "${missing} prerequisito/i di sistema non trovato/i."
        error "Questo script richiede un sistema Debian/Ubuntu con apt-get e systemd."
        exit 1
    fi

    log "✔ Prerequisiti di sistema verificati."
}