# =============================================================================
# nginx reverse proxy for Odoo.
#
# placeholders substituted when the vhost is rendered:
#   {{NGINX_SERVER_NAME}}   domain name or IP
#   {{ODOO_PORT}}           Odoo's local port (default 8069)
#   {{NGINX_CLIENT_MAX}}    maximum upload body size (default 100m)
#   {{INSTANCE_BASE}}       this instance's name, for the log filenames
#
# TLS: this vhost listens on **port 80 only**, deliberately.
#
# the supported way to get HTTPS is `certbot --nginx`, which obtains the
# certificates and **rewrites this vhost itself**, adding the 443 block and the
# redirect. generating a 443 block here would compete with it — and pointing at
# non-existent certificates would fail validation, and with it the whole
# installation.
#
# `--open-https-port` opens 443 on the firewall ahead of that step; it does not
# touch this file. it was called `--enable-ssl` and promised what it did not do
# (A-V3-6).
# =============================================================================

# -- upstream -----------------------------------------------------------------
upstream odoo {
    server 127.0.0.1:{{ODOO_PORT}};
    keepalive 16;
}

upstream odoo-longpolling {
    server 127.0.0.1:8072;
}

# -- HTTP (port 80) -----------------------------------------------------------
server {
    listen 80;
    listen [::]:80;
    server_name {{NGINX_SERVER_NAME}};

    # uncomment once TLS is in place to redirect all HTTP traffic.
    # return 301 https://$host$request_uri;

    # one file per version: two instances on one machine must not write over
    # each other (A-V3-12). they survive the rollback, being logs, but at least
    # one can tell whose they are.
    access_log  /var/log/nginx/{{INSTANCE_BASE}}.access.log;
    error_log   /var/log/nginx/{{INSTANCE_BASE}}.error.log;

    # maximum upload size (invoices, attachments)
    client_max_body_size {{NGINX_CLIENT_MAX}};

    # security headers
    add_header X-Frame-Options         SAMEORIGIN;
    add_header X-Content-Type-Options  nosniff;
    add_header X-XSS-Protection        "1; mode=block";
    add_header Referrer-Policy         "strict-origin-when-cross-origin";

    # -- longpolling (real-time notifications) --------------------------------
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

    # -- static content, aggressively cached ----------------------------------
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

    # -- main proxy -----------------------------------------------------------
    location / {
        proxy_pass         http://odoo;
        proxy_http_version 1.1;
        proxy_set_header   Host              $http_host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;

        # generous timeouts for heavy work (imports, reports)
        proxy_read_timeout  720s;
        proxy_send_timeout  720s;
        proxy_connect_timeout 30s;

        # buffering off, for streaming downloads such as backups
        proxy_buffering off;

        # required for chunked transfer encoding
        proxy_set_header   Connection "";
    }

    # -- deny direct access to Odoo's internal files --------------------------
    location ~* \.(py|pyc|cfg|conf)$ {
        deny all;
    }
}
