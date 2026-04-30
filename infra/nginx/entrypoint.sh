#!/bin/sh
set -e

# Substitute only ${BACKEND_URL} to avoid clobbering nginx $variable syntax
envsubst '${BACKEND_URL}' \
  < /etc/nginx/templates/default.conf.template \
  > /etc/nginx/conf.d/default.conf

exec nginx -g "daemon off;"
