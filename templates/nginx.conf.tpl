# =============================================================================
# templates/nginx.conf.tpl — Reverse proxy Nginx per Odoo
# Placeholder sostituiti da `nginx_write_config::render_vhost`:
#   {{NGINX_SERVER_NAME}}   nome di dominio o IP (es. odoo.example.com)
#   {{ODOO_PORT}}           porta locale di Odoo (default 8069)
#   {{NGINX_CLIENT_MAX}}    dimensione massima body upload (default 100m)
# =============================================================================
#
# TLS: questo vhost ascolta **solo sulla porta 80**, di proposito.
#
# Il modo supportato per ottenere HTTPS è `certbot --nginx`, che ottiene i
# certificati e **riscrive questo vhost da sé**, aggiungendo il blocco su 443 e
# il redirect da 80. Generare qui un blocco 443 significherebbe competere con
# lui — e, se puntasse a certificati inesistenti, `nginx -t` fallirebbe e con
# esso l'intera installazione.
#
# Il flag `--open-https-port` apre la 443 sul firewall in vista di quel
# passaggio; non tocca questo file. Si chiamava `--enable-ssl` e prometteva
# quello che non faceva (A-V3-6).
# =============================================================================

# ── Upstream Odoo ─────────────────────────────────────────────────────────────
upstream odoo {
    server 127.0.0.1:{{ODOO_PORT}};
    keepalive 16;
}

upstream odoo-longpolling {
    server 127.0.0.1:8072;
}

# ── HTTP (porta 80) ───────────────────────────────────────────────────────────
server {
    listen 80;
    listen [::]:80;
    server_name {{NGINX_SERVER_NAME}};

    # Se SSL è attivo redirige tutto il traffico HTTP su HTTPS.
    # Commentare o rimuovere questo blocco per servire Odoo solo in HTTP.
    # return 301 https://$host$request_uri;

    access_log  /var/log/nginx/odoo18.access.log;
    error_log   /var/log/nginx/odoo18.error.log;

    # Dimensione massima dei file caricati (fatture, allegati, ecc.)
    client_max_body_size {{NGINX_CLIENT_MAX}};

    # Header di sicurezza
    add_header X-Frame-Options         SAMEORIGIN;
    add_header X-Content-Type-Options  nosniff;
    add_header X-XSS-Protection        "1; mode=block";
    add_header Referrer-Policy         "strict-origin-when-cross-origin";

    # ── Longpolling (notifiche real-time, bus) ────────────────────────────────
    location /web/websocket {
        proxy_pass         http://odoo-longpolling;
        proxy_http_version 1.1;
        proxy_set_header   Upgrade    $http_upgrade;
        proxy_set_header   Connection "upgrade";
        proxy_set_header   Host       $host;
        proxy_set_header   X-Real-IP  $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    location /longpolling {
        proxy_pass         http://odoo-longpolling;
        proxy_http_version 1.1;
        proxy_set_header   Host               $host;
        proxy_set_header   X-Real-IP          $remote_addr;
        proxy_set_header   X-Forwarded-For    $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto  $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # ── Contenuto statico con cache aggressiva ────────────────────────────────
    location ~* /web/static/ {
        proxy_pass         http://odoo;
        proxy_cache_valid  200 90d;
        proxy_buffering    on;
        expires            864000;
        add_header         Cache-Control "public, immutable";
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
    }

    # ── Proxy principale ──────────────────────────────────────────────────────
    location / {
        proxy_pass         http://odoo;
        proxy_http_version 1.1;
        proxy_set_header   Host              $http_host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;

        # Timeout generosi per operazioni pesanti (importazioni, report)
        proxy_read_timeout  720s;
        proxy_send_timeout  720s;
        proxy_connect_timeout 30s;

        # Disabilita il buffering per lo streaming (es. download di backup)
        proxy_buffering off;

        # Necessario per proxy_pass con header chunked
        proxy_set_header   Connection "";
    }

    # ── Blocca l'accesso diretto ai file di sistema Odoo ─────────────────────
    location ~* \.(py|pyc|cfg|conf)$ {
        deny all;
    }
}
