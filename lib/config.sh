#!/usr/bin/env bash
# lib/config.sh — Generazione odoo.conf da template
# Parte di odoo-autoinstaller
# Sourced da install.sh; non eseguire direttamente.
# ──────────────────────────────────────────────────────────────────────────────
set -euo pipefail

# ──────────────────────────────────────────────────────────────────────────────
# Valori di default per le variabili di configurazione Odoo.
# Tutte le variabili sono sovrascrivibili da configs/*.env prima del source.
# ──────────────────────────────────────────────────────────────────────────────
_config_set_defaults() {
    # Sicurezza
    : "${ODOO_ADMIN_PASSWD:=admin}"

    # Database
    : "${DB_HOST:=False}"
    : "${DB_PORT:=False}"
    : "${DB_PASSWORD:=False}"
    : "${DB_NAME:=False}"           # False = Odoo sceglie il db dalla UI

    # HTTP
    : "${ODOO_HTTP_INTERFACE:=0.0.0.0}"
    : "${ODOO_PROXY_MODE:=False}"   # True se dietro nginx/apache

    # Paths
    : "${ODOO_HOME:=/opt/odoo}"
    : "${ODOO_VERSION:=18}"
    : "${ODOO_INSTALL_DIR:=${ODOO_HOME}/odoo${ODOO_VERSION}}"
    : "${ODOO_ADDONS_PATH:=${ODOO_INSTALL_DIR}/odoo/odoo/addons,${ODOO_INSTALL_DIR}/odoo/addons,${ODOO_INSTALL_DIR}/repos/modules}"
    : "${ODOO_DATA_DIR:=${ODOO_HOME}/.local/share/Odoo}"
    # Vuoto di default: Odoo logga su stdout/stderr del processo/service.
    : "${ODOO_LOGFILE:=}"
    : "${ODOO_CONF_DIR:=${ODOO_INSTALL_DIR}}"

    # Worker / performance
    : "${ODOO_WORKERS:=0}"
    : "${ODOO_MAX_CRON_THREADS:=1}"

    # Limits (valori Odoo upstream per ambienti production)
    : "${ODOO_LIMIT_MEMORY_HARD:=2684354560}"   # 2.5 GB
    : "${ODOO_LIMIT_MEMORY_SOFT:=2147483648}"   # 2 GB
    : "${ODOO_LIMIT_REQUEST:=8192}"
    : "${ODOO_LIMIT_TIME_CPU:=60}"
    : "${ODOO_LIMIT_TIME_REAL:=120}"

    # Log
    : "${ODOO_LOG_LEVEL:=info}"

    export ODOO_ADMIN_PASSWD DB_HOST DB_PORT DB_USER DB_PASSWORD DB_NAME
    export ODOO_HTTP_INTERFACE ODOO_PORT ODOO_PROXY_MODE
    export ODOO_ADDONS_PATH ODOO_DATA_DIR ODOO_LOGFILE
    export ODOO_WORKERS ODOO_MAX_CRON_THREADS
    export ODOO_LIMIT_MEMORY_HARD ODOO_LIMIT_MEMORY_SOFT
    export ODOO_LIMIT_REQUEST ODOO_LIMIT_TIME_CPU ODOO_LIMIT_TIME_REAL
    export ODOO_LOG_LEVEL ODOO_VERSION
}

# ──────────────────────────────────────────────────────────────────────────────
# _config_validate_template <tpl_path>
#   Verifica che il template esista e non sia vuoto.
# ──────────────────────────────────────────────────────────────────────────────
_config_validate_template() {
    local tpl="$1"

    if [[ ! -f "$tpl" ]]; then
        error "Template non trovato: ${tpl}"
        return 1
    fi

    if [[ ! -s "$tpl" ]]; then
        error "Template vuoto: ${tpl}"
        return 1
    fi
}

# ──────────────────────────────────────────────────────────────────────────────
# _config_validate_conf <conf_path>
#   Controlli minimali di coerenza sul .conf generato:
#   - sezione [options] presente
#   - addons_path valorizzato
#   - http_port valorizzato
# ──────────────────────────────────────────────────────────────────────────────
_config_validate_conf() {
    local conf="$1"
    local ok=true

    grep -q '^\[options\]' "$conf" || { warn "odoo.conf: sezione [options] mancante"; ok=false; }
    grep -q '^addons_path' "$conf" || { warn "odoo.conf: addons_path non trovato";    ok=false; }
    grep -q '^http_port'   "$conf" || { warn "odoo.conf: http_port non trovato";      ok=false; }

    # Rileva placeholder non sostituiti (es. ${QUALCOSA})
    if grep -qE '\$\{[A-Z_]+\}' "$conf"; then
        local unresolved
        unresolved=$(grep -oE '\$\{[A-Z_]+\}' "$conf" | sort -u | tr '\n' ' ')
        warn "Placeholder non sostituiti in odoo.conf: ${unresolved}"
        ok=false
    fi

    [[ "$ok" == true ]]
}

# ──────────────────────────────────────────────────────────────────────────────
# _config_prepare_log_dir
#   Crea la directory di log e ne assegna la proprietà all'utente Odoo.
# ──────────────────────────────────────────────────────────────────────────────
_config_prepare_log_dir() {
    if [[ -z "${ODOO_LOGFILE}" ]]; then
        log "ODOO_LOGFILE vuoto: salto preparazione directory log file."
        return 0
    fi

    local log_dir
    log_dir="$(dirname "${ODOO_LOGFILE}")"

    if [[ ! -d "$log_dir" ]]; then
        log "Creazione directory log: ${log_dir}"
        sudo mkdir -p "$log_dir"
    fi

    sudo chown -R "${ODOO_USER}:${ODOO_USER}" "$log_dir"
    sudo chmod 750 "$log_dir"
}

# ──────────────────────────────────────────────────────────────────────────────
# _config_prepare_data_dir
#   Crea la data_dir di Odoo e ne assegna la proprietà.
# ──────────────────────────────────────────────────────────────────────────────
_config_prepare_data_dir() {
    if [[ ! -d "${ODOO_DATA_DIR}" ]]; then
        log "Creazione data_dir: ${ODOO_DATA_DIR}"
        sudo -u "${ODOO_USER}" mkdir -p "${ODOO_DATA_DIR}"
    fi
}

# ──────────────────────────────────────────────────────────────────────────────
# _config_render_template <tpl_path> <dest_path>
#   Sostituisce i placeholder {{VAR}} (o ${VAR} con envsubst).
#   Strategia:
#     1. envsubst  → sostituzione semplice delle variabili di ambiente esportate
#     2. sed finale → rimuove eventuali righe con placeholder residui non risolti
#                     (comportamento sicuro: li lascia commentati)
# ──────────────────────────────────────────────────────────────────────────────
_config_render_template() {
    local tpl="$1"
    local dest="$2"
    local tmp
    tmp="$(mktemp)"

    log "Rendering template: ${tpl} → ${dest}"

    # envsubst sostituisce solo le variabili esplicitamente elencate,
    # evitando di alterare eventuali $(...) o $VAR non attinenti al template.
    local vars
    vars=$(grep -oE '\$\{[A-Z_]+\}' "$tpl" | sort -u | tr '\n' ' ')

    # shellcheck disable=SC2016
    envsubst "$vars" < "$tpl" > "$tmp"

    # Se ODOO_LOGFILE e' vuoto, disabilita esplicitamente la direttiva logfile
    # lasciando traccia nel file generato.
    if [[ -z "${ODOO_LOGFILE}" ]]; then
        sed -i 's|^logfile[[:space:]]*=.*$|; logfile disabled: using stdout\/stderr|' "$tmp"
    fi

    # Sposta il file nella destinazione con permessi corretti
    sudo mv "$tmp" "$dest"
    sudo chown "${ODOO_USER}:${ODOO_USER}" "$dest"
    sudo chmod 640 "$dest"   # leggibile solo da odoo e root
}

# ──────────────────────────────────────────────────────────────────────────────
# generate_config  ← funzione pubblica chiamata da install.sh
#
#   Flusso:
#     1. Imposta i default
#     2. Prepara log e data dir
#     3. Valida il template
#     4. Esegue il rendering
#     5. Valida il .conf prodotto
#     6. Mostra un riepilogo (no password in chiaro)
# ──────────────────────────────────────────────────────────────────────────────
generate_config() {
    # ODOO_VERSION_SHORT (es. "18") viene usato nel nome del file
    # odoo18.conf, referenziato poi dalla unit systemd.
    local version_short="${ODOO_VERSION%%.*}"

    log "━━━ Generazione odoo${version_short}.conf ━━━"

    _config_set_defaults

    local tpl="${TEMPLATES_DIR}/odoo.conf.tpl"
    local conf="${ODOO_CONF_DIR}/odoo${version_short}.conf"

    _config_validate_template "$tpl"

    _config_prepare_log_dir
    _config_prepare_data_dir

    # Idempotenza: backup se esiste già
    if [[ -f "$conf" ]]; then
        local backup
        backup="${conf}.bak.$(date +%Y%m%d%H%M%S)"
        warn "File esistente trovato — backup in: ${backup}"
        sudo cp "$conf" "$backup"
    fi

    _config_render_template "$tpl" "$conf"

    if ! _config_validate_conf "$conf"; then
        error "Validazione odoo.conf fallita. Controllare il file: ${conf}"
        return 1
    fi

    # ── Riepilogo (nessun segreto stampato in chiaro) ──────────────────────
    log "odoo.conf generato con successo: ${conf}"
    log "  addons_path : ${ODOO_ADDONS_PATH}"
    log "  http_port   : ${ODOO_PORT}"
    log "  db_user     : ${DB_USER}"
    log "  db_name     : ${DB_NAME:-<scelto dalla UI>}"
    if [[ -n "${ODOO_LOGFILE}" ]]; then
        log "  logfile     : ${ODOO_LOGFILE}"
    else
        log "  logfile     : <disabled, stdout/stderr>"
    fi
    log "  workers     : ${ODOO_WORKERS}"
    log "  log_level   : ${ODOO_LOG_LEVEL}"
}