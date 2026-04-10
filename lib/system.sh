#!/usr/bin/env bash
# =============================================================================
# lib/system.sh — Creazione utente Odoo e installazione dipendenze di sistema
#
# Variabili attese (esportate da install.sh):
#   ODOO_USER   — nome utente di sistema per Odoo  (default: odoo)
#   ODOO_HOME   — home directory dell'utente        (default: /opt/odoo) 
# ============================================================================= 
set -euo pipefail

# -----------------------------------------------------------------------------
# _apt_packages_odoo
#   Lista canoniche dei pacchetti apt richiesti da Odoo 18.
#   Separata dalla funzione di installazione per facilitare test e override.
# -----------------------------------------------------------------------------
_apt_packages_odoo() {
    cat <<'EOF'
git
curl
wget
python3-pip
python3-dev
python3-venv
python3-wheel
python3-setuptools
build-essential
gettext-base
libfreetype6-dev
libxml2-dev
libzip-dev
libldap2-dev
libsasl2-dev
node-less
libjpeg-dev
zlib1g-dev
libpq-dev
libxslt1-dev
libtiff5-dev
libjpeg8-dev
libopenjp2-7-dev
liblcms2-dev
libwebp-dev
libharfbuzz-dev
libfribidi-dev
libxcb1-dev
libev-dev
libc-ares-dev
EOF
}

# -----------------------------------------------------------------------------
# install_dependencies
#   Aggiorna l'indice apt e installa tutti i pacchetti di sistema necessari.
#   Idempotente: apt-get install è no-op se i pacchetti sono già presenti.
# -----------------------------------------------------------------------------
install_dependencies() {
    log "Aggiornamento indice dei pacchetti apt…"
    apt-get update -qq

    # Costruiamo l'array dei pacchetti dalla lista canonica
    local -a pkgs
    mapfile -t pkgs < <(_apt_packages_odoo)

    log "Installazione dipendenze di sistema (${#pkgs[@]} pacchetti)…"
    # DEBIAN_FRONTEND=noninteractive evita prompt interattivi (es. tzdata)
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        "${pkgs[@]}" 2>&1 | _apt_progress_filter

    log "Dipendenze di sistema installate con successo."
}

# -----------------------------------------------------------------------------
# _apt_progress_filter
#   Filtra l'output verboso di apt lasciando passare solo le righe significative.
#   In questo modo il log rimane leggibile senza perdere errori reali.
# -----------------------------------------------------------------------------
_apt_progress_filter() {
    grep -E '(^Get:|^Unpacking|^Setting up|^Processing|ERROR|warning)' || true
}

# -----------------------------------------------------------------------------
# create_odoo_user
#   Crea l'utente di sistema dedicato a Odoo se non esiste già.
#
#   Scelte architetturali — principio del privilegio minimo:
#     --system       → UID < 1000, account di sistema senza password
#     --user-group   → gruppo dedicato, separato dai gruppi di sistema
#     --shell /bin/false → NESSUNA shell interattiva (produzione):
#                         • systemd esegue ExecStart via setuid() — non serve bash
#                         • sudo -u odoo <cmd> funziona senza shell utente
#                         • se l'account fosse compromesso, nessun terminale disponibile
#     --home-dir /opt/odoo → standard FHS per software applicativo di terze parti;
#                            separa il codice (/opt) dai dati di sistema (/var/lib/odoo)
#     --create-home  → crea /opt/odoo con owner corretto al primo avvio
# -----------------------------------------------------------------------------
create_odoo_user() {
    local user="${ODOO_USER:?La variabile ODOO_USER non è impostata}"
    local home="${ODOO_HOME:?La variabile ODOO_HOME non è impostata}"

    if id "${user}" &>/dev/null; then
        warn "L'utente '${user}' esiste già — creazione saltata."
        _verify_odoo_user_homedir "${user}" "${home}"
        return 0
    fi

    log "Creazione utente di sistema '${user}' con home '${home}'…"
    useradd \
        --system \
        --create-home \
        --home-dir  "${home}" \
        --user-group \
        --shell     /bin/false \
        "${user}"

    log "Utente '${user}' creato con successo."
    _verify_odoo_user_homedir "${user}" "${home}"
}

# -----------------------------------------------------------------------------
# _verify_odoo_user_homedir
#   Assicura che la home directory esista e abbia i permessi corretti.
#   Chiamata sia alla creazione sia quando l'utente era già presente.
# -----------------------------------------------------------------------------
_verify_odoo_user_homedir() {
    local user="$1"
    local home="$2"

    if [[ ! -d "${home}" ]]; then
        log "Creazione directory home '${home}' (mancante)…"
        mkdir -p "${home}"
    fi

    # Verifica ownership: la home deve appartenere all'utente odoo
    local current_owner
    current_owner=$(stat -c '%U' "${home}")
    if [[ "${current_owner}" != "${user}" ]]; then
        log "Impostazione ownership '${user}:${user}' su '${home}'…"
        chown "${user}:${user}" "${home}"
    fi

    # Permessi minimi: 750 (owner rwx, group r-x, other ---)
    chmod 750 "${home}"
    log "Home directory '${home}' verificata (owner: ${user}, perms: 750)."
}

# -----------------------------------------------------------------------------
# setup_log_dir
#   Crea /var/log/odoo con i permessi corretti per il logfile definito in
#   odoo.conf (logfile = /var/log/odoo/odoo18.log).
# -----------------------------------------------------------------------------
setup_log_dir() {
    local log_dir="/var/log/odoo"
    local user="${ODOO_USER:?La variabile ODOO_USER non è impostata}"

    if [[ -d "${log_dir}" ]]; then
        warn "Directory log '${log_dir}' già esistente — skip creazione."
    else
        log "Creazione directory log '${log_dir}'…"
        mkdir -p "${log_dir}"
    fi

    chown "${user}:${user}" "${log_dir}"
    chmod 750 "${log_dir}"
    log "Directory log '${log_dir}' pronta (owner: ${user})."
}

# -----------------------------------------------------------------------------
# install_wkhtmltopdf
#   Scarica e installa wkhtmltopdf con patch Qt da GitHub releases.
#
#   Il pacchetto apt di Ubuntu (0.12.6 senza Qt patch) genera PDF difettosi
#   con Odoo (header/footer mancanti, caratteri errati). La versione ufficiale
#   di Odoo richiede la build "0.12.6.1-3" compilata con Qt patchato.
#
#   Mappa codename → pacchetto (GitHub releases wkhtmltopdf/packaging):
#     noble  (24.04) → usa jammy (compatibile, nessun pacchetto native)
#     jammy  (22.04) → jammy
#     focal  (20.04) → focal
#     bookworm (deb12) → bookworm
#     bullseye (deb11) → bullseye
#
#   Idempotente: se già installata la versione corretta, esce senza fare nulla.
# -----------------------------------------------------------------------------
install_wkhtmltopdf() {
    local wk_version="0.12.6.1-3"
    local wk_base_url="https://github.com/wkhtmltopdf/packaging/releases/download/${wk_version}"

    # ── Idempotenza ───────────────────────────────────────────────────────────
    if command -v wkhtmltopdf &>/dev/null; then
        local installed_ver
        installed_ver=$(wkhtmltopdf --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
        if [[ "${installed_ver}" == "0.12.6.1" ]]; then
            log "wkhtmltopdf ${installed_ver} (Qt patch) già presente — skip."
            return 0
        else
            warn "wkhtmltopdf trovato ma versione '${installed_ver}' (attesa 0.12.6.1 con Qt patch)."
            warn "Procedo con l'installazione della versione corretta."
        fi
    fi

    # ── Determina il pacchetto in base al codename ───────────────────────────
    local codename="${OS_CODENAME:-}"
    if [[ -z "${codename}" ]]; then
        codename=$(grep -E '^VERSION_CODENAME=' /etc/os-release | cut -d= -f2 | tr -d '"')
    fi

    local pkg_suffix
    case "${codename}" in
        noble|mantic|lunar)  pkg_suffix="jammy"    ;;   # nessun pacchetto native, jammy è compatibile
        jammy)               pkg_suffix="jammy"    ;;
        focal)               pkg_suffix="focal"    ;;
        bookworm)            pkg_suffix="bookworm" ;;
        bullseye)            pkg_suffix="bullseye" ;;
        *)
            warn "Codename '${codename}' non mappato — uso pacchetto jammy come fallback."
            pkg_suffix="jammy"
            ;;
    esac

    local pkg_name="wkhtmltox_${wk_version}.${pkg_suffix}_amd64.deb"
    local pkg_url="${wk_base_url}/${pkg_name}"
    local tmp_deb
    tmp_deb="$(mktemp --suffix=.deb)"
    # Rimuovi il file temporaneo all'uscita (successo o errore)
    trap "rm -f '${tmp_deb}'" RETURN

    log "Download wkhtmltopdf ${wk_version} (${pkg_suffix})…"
    log "  URL: ${pkg_url}"

    if ! wget -q --show-progress -O "${tmp_deb}" "${pkg_url}"; then
        error "Download wkhtmltopdf fallito. Verifica la connessione o scarica manualmente da:"
        error "  ${pkg_url}"
        return 1
    fi

    log "Installazione wkhtmltopdf…"
    # dpkg -i può fallire per dipendenze mancanti; apt-get -f install le risolve.
    dpkg -i "${tmp_deb}" || true
    DEBIAN_FRONTEND=noninteractive apt-get install -f -y --no-install-recommends

    # ── Verifica post-installazione ───────────────────────────────────────────
    if ! command -v wkhtmltopdf &>/dev/null; then
        error "wkhtmltopdf non trovato dopo l'installazione."
        return 1
    fi

    local final_ver
    final_ver=$(wkhtmltopdf --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)
    log "✔ wkhtmltopdf ${final_ver} installato con successo."
}