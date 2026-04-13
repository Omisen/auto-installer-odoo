#!/usr/bin/env bash
# =============================================================================
# lib/cli.sh — Raccolta input, validazione e normalizzazione parametri installer
# =============================================================================
# Funzioni esposte:
#   collect_inputs                 → prompt interattivo con fallback ai default
#   validate_selected_inputs       → valida e normalizza i valori raccolti
#   print_installation_configuration → stampa il riepilogo finale configurazione
# =============================================================================

set -euo pipefail

SUPPORTED_ODOO_VERSIONS=("16.0" "17.0" "18.0" "19.0")

build_default_install_dir() {
    local home="$1"
    local version="$2"

    echo "${home}/odoo${version%%.*}"
}

normalize_odoo_version() {
    local value="$1"

    case "$value" in
        16|17|18|19)
            echo "${value}.0"
            ;;
        16.0|17.0|18.0|19.0)
            echo "$value"
            ;;
        *)
            return 1
            ;;
    esac
}

validate_identifier() {
    local value="$1"

    [[ "$value" =~ ^[A-Za-z0-9._-]+$ ]] || return 1
    echo "$value"
}

validate_port_value() {
    local value="$1"

    [[ "$value" =~ ^[0-9]+$ ]] || return 1
    (( value >= 1 && value <= 65535 )) || return 1
    echo "$value"
}

validate_absolute_path() {
    local value="$1"

    [[ "$value" == /* ]] || return 1
    echo "$value"
}

validate_install_dir_scope() {
    local value="$1"
    local home="$2"

    [[ "$value" == "$home" || "$value" == "$home/"* ]] || return 1
    echo "$value"
}

is_interactive_input_available() {
    [[ -t 0 && -t 1 ]]
}

prompt_value_with_default() {
    local var_name="$1"
    local label="$2"
    local suggested_value="$3"
    local validator="$4"
    local hint="${5:-}"
    local prompt_suffix=""
    local input_value=""
    local validated_value=""

    if [[ -n "$hint" ]]; then
        prompt_suffix=" ${hint}"
    fi

    while true; do
        read -r -p "${label}${prompt_suffix} [${suggested_value}]: " input_value

        if [[ -z "$input_value" ]]; then
            printf -v "$var_name" '%s' "$suggested_value"
            log "${label}: nessun valore inserito, uso '${suggested_value}'."
            return 0
        fi

        if validated_value="$("$validator" "$input_value")"; then
            printf -v "$var_name" '%s' "$validated_value"
            log "${label}: impostato '${validated_value}'."
            return 0
        fi

        warn "Valore non valido per ${label}. Riprova."
    done
}

prompt_admin_password() {
    local suggested_value="$1"
    local input_value=""
    local prompt_hint="$DEFAULT_ODOO_ADMIN_PASSWD"
    local keep_message="uso il default previsto"

    if [[ "$suggested_value" != "$DEFAULT_ODOO_ADMIN_PASSWD" ]]; then
        prompt_hint="valore configurato"
        keep_message="mantengo il valore configurato"
    fi

    while true; do
        read -r -s -p "Password admin Odoo [${prompt_hint}]: " input_value
        echo ""

        if [[ -z "$input_value" ]]; then
            ODOO_ADMIN_PASSWD="$suggested_value"
            log "Password admin Odoo: nessun valore inserito, ${keep_message}."
            return 0
        fi

        ODOO_ADMIN_PASSWD="$input_value"
        log "Password admin Odoo personalizzata impostata."
        return 0
    done
}

sync_install_paths() {
    local derived_install_dir

    derived_install_dir="$(build_default_install_dir "$ODOO_HOME" "$ODOO_VERSION")"
    if [[ -z "${ODOO_INSTALL_DIR:-}" ]]; then
        ODOO_INSTALL_DIR="$derived_install_dir"
    fi
}

validate_selected_inputs() {
    ODOO_VERSION="$(normalize_odoo_version "$ODOO_VERSION")" || \
        error "Versione Odoo non valida: '${ODOO_VERSION}'. Valori ammessi: ${SUPPORTED_ODOO_VERSIONS[*]}"
    ODOO_USER="$(validate_identifier "$ODOO_USER")" || \
        error "Utente Odoo non valido: '${ODOO_USER}'. Usa solo lettere, numeri, punto, trattino o underscore."
    ODOO_PORT="$(validate_port_value "$ODOO_PORT")" || \
        error "Porta Odoo non valida: '${ODOO_PORT}'. Inserisci un numero tra 1 e 65535."
    DB_NAME="$(validate_identifier "$DB_NAME")" || \
        error "Nome database non valido: '${DB_NAME}'. Usa solo lettere, numeri, punto, trattino o underscore."

    if [[ -z "${DB_USER:-}" ]] || { [[ "$CLI_DB_USER_SET" != true ]] && [[ "$DB_USER" == "$DEFAULT_ODOO_USER" ]]; }; then
        DB_USER="$ODOO_USER"
    else
        DB_USER="$(validate_identifier "$DB_USER")" || \
            error "Utente database non valido: '${DB_USER}'. Usa solo lettere, numeri, punto, trattino o underscore."
    fi

    sync_install_paths
    ODOO_INSTALL_DIR="$(validate_absolute_path "$ODOO_INSTALL_DIR")" || \
        error "Install dir non valida: '${ODOO_INSTALL_DIR}'. Inserisci un percorso assoluto."
    ODOO_INSTALL_DIR="$(validate_install_dir_scope "$ODOO_INSTALL_DIR" "$ODOO_HOME")" || \
        error "Install dir non valida: '${ODOO_INSTALL_DIR}'. Deve essere sotto '${ODOO_HOME}'."

    [[ -n "$ODOO_ADMIN_PASSWD" ]] || error "La password admin Odoo non puo' essere vuota."
}

collect_main_inputs() {
    local suggested_version="$ODOO_VERSION"
    local suggested_install_dir="${ODOO_INSTALL_DIR:-}"
    local suggested_admin_password="$ODOO_ADMIN_PASSWD"

    if [[ "$CLI_ODOO_VERSION_SET" == true ]]; then
        log "Versione Odoo da CLI: ${ODOO_VERSION}"
    else
        prompt_value_with_default ODOO_VERSION "Versione Odoo" "$suggested_version" normalize_odoo_version "16.0/17.0/18.0/19.0"
    fi

    if [[ "$CLI_ODOO_USER_SET" == true ]]; then
        log "Utente Odoo da CLI: ${ODOO_USER}"
    else
        prompt_value_with_default ODOO_USER "Utente Odoo" "$ODOO_USER" validate_identifier
    fi

    if [[ "$CLI_DB_NAME_SET" == true ]]; then
        log "Database Odoo da CLI: ${DB_NAME}"
    else
        prompt_value_with_default DB_NAME "Database Odoo" "$DB_NAME" validate_identifier
    fi

    if [[ "$CLI_ODOO_PORT_SET" == true ]]; then
        log "Porta Odoo da CLI: ${ODOO_PORT}"
    else
        prompt_value_with_default ODOO_PORT "Porta Odoo" "$ODOO_PORT" validate_port_value
    fi

    if [[ -z "$suggested_install_dir" ]]; then
        suggested_install_dir="$(build_default_install_dir "$ODOO_HOME" "$ODOO_VERSION")"
    fi

    if [[ "$CLI_ODOO_INSTALL_DIR_SET" == true ]]; then
        log "Install dir da CLI: ${ODOO_INSTALL_DIR}"
    else
        prompt_value_with_default ODOO_INSTALL_DIR "Install dir Odoo" "$suggested_install_dir" validate_absolute_path "(sotto ${ODOO_HOME})"
    fi

    if [[ "$CLI_ODOO_ADMIN_PASSWD_SET" == true ]]; then
        log "Password admin Odoo acquisita da CLI."
    else
        prompt_admin_password "$suggested_admin_password"
    fi
}

collect_inputs() {
    if ! is_interactive_input_available; then
        log "Input interattivo non disponibile: uso valori da CLI, .env e default finali."
        return 0
    fi

    echo ""
    echo -e "${GREEN}Configurazione installazione Odoo${NC}"
    echo "Premi Invio per confermare il valore suggerito oppure inseriscine uno diverso."
    echo ""

    collect_main_inputs
}

print_installation_configuration() {
    echo ""
    log "Configurazione finale installazione:"
    log "  Versione Odoo : ${ODOO_VERSION}"
    log "  Utente Odoo   : ${ODOO_USER}"
    log "  Database      : ${DB_NAME}"
    log "  DB user       : ${DB_USER}"
    log "  Porta HTTP    : ${ODOO_PORT}"
    log "  Install dir   : ${ODOO_INSTALL_DIR}"
    if [[ "$ODOO_ADMIN_PASSWD" == "$DEFAULT_ODOO_ADMIN_PASSWD" ]]; then
        warn "  Admin passwd  : default '${DEFAULT_ODOO_ADMIN_PASSWD}'"
    else
        log "  Admin passwd  : personalizzata"
    fi
}
