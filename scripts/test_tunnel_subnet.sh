#!/usr/bin/env bash
# shellcheck disable=SC2030,SC2031
set -euo pipefail
cd "$(dirname "$0")/.."
# Load definitions without invoking the privileged installer entry point.
# shellcheck disable=SC1090
source <(sed '$d' app/src/main/assets/deploy.sh)
trap - ERR
die() { printf '%s\n' "$1" >&2; exit 2; }
for value in 10.77.88.0/24 172.23.45.0/24 192.168.99.0/24; do
    (export CSQTT_TUN_SUBNET="$value"; configure_tunnel_subnet; [ "$CSQTT_TUN_SUBNET" = "$value" ])
done
for value in 8.8.8.0/24 10.1.2.1/24 10.1.0.0/16 '10.1.2.0/24;id' 10.999.1.0/24; do
    if (export CSQTT_TUN_SUBNET="$value"; configure_tunnel_subnet) 2>/dev/null; then
        printf 'accepted invalid subnet: %s\n' "$value" >&2
        exit 1
    fi
done
printf 'Tunnel subnet validation: OK\n'
