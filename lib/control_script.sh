#!/usr/bin/env bash
# =============================================================================
# lib/control_script.sh — Install local Odoo control helper script
#
# Public functions:
#   install_odoo_control_script — installs ~/.scripts/odoo.sh and related PATH
#
# Depends on globals/functions from installer.sh:
#   - set -euo pipefail inherited from caller
# =============================================================================

set -euo pipefail

install_odoo_control_script() {
    local scripts_dir="${HOME}/.scripts"
    local control_script="${scripts_dir}/odoo.sh"
    local local_bin_dir="${HOME}/.local/bin"
    local bashrc_file="${HOME}/.bashrc"
    local path_export='export PATH="$HOME/.local/bin:$PATH"'

    mkdir -p "${scripts_dir}"
    cat <<'EOF' > "${control_script}"
#!/usr/bin/env bash

set -euo pipefail

usage() {
  echo "Usage: odoo {start|stop|restart|dev}"
}

case "${1:-}" in
  start)
    sudo systemctl start odoo
    ;;
  stop)
    sudo systemctl stop odoo
    ;;
  restart)
    sudo systemctl restart odoo
    ;;
  dev)
    sudo systemctl stop odoo
    sudo su - odoo -s /bin/bash
    ;;
  status)
    sudo systemctl status odoo
    ;;
  *)
    usage
    ;;
esac
EOF

    chmod +x "${control_script}"

    if [[ ! -d "${local_bin_dir}" ]]; then
      mkdir -p "${local_bin_dir}"
    fi
    
    ln -sf "${control_script}" "${local_bin_dir}/odoo"

    if [[ ! -f "${bashrc_file}" ]]; then
        touch "${bashrc_file}"
    fi

    if ! grep -Fqx "${path_export}" "${bashrc_file}"; then
        echo "${path_export}" >> "${bashrc_file}"
        log "TIP: Esecuzione di: source ~/.bashrc per applicazione modifiche"
    fi
}