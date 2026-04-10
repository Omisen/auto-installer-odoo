#!/usr/bin/env bash
# =============================================================================
# lib/system.sh — Creazione utente Odoo e installazione dipendenze di sistema
#
# Variabili attese (esportate da install.sh):
#   ODOO_USER   — nome utente di sistema per Odoo  (default: odoo)
#   ODOO_HOME   — home directory dell'utente        (default: /opt/odoo) #FIX this shoud be /home/odoo 
# ============================================================================= #BUG in checks.sh
set -euo pipefail

# -----------------------------------------------------------------------------
# _apt_packages_odoo
#   Lista canoniche dei pacchetti apt richiesti da Odoo 18.
#   Separata dalla funzione di installazione per facilitare test e override.
# -----------------------------------------------------------------------------
_apt_packages_odoo() {
    cat <<'EOF'
git
python3-pip
python3-dev
python3-venv
python3-wheel
python3-setuptools
build-essential
wget
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
#   Scelte di sicurezza:
#     -r  → account di sistema (UID < 1000, non compare nel login screen)
#     -U  → crea un gruppo omonimo
#     -s /bin/false → nessuna shell interattiva (non accedibile via su/ssh)
#     -d ODOO_HOME  → home impostata ma non ancora creata (-M sarebbe no-home)
#     -m  → crea la home directory se non esiste
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