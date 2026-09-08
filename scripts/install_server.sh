#!/usr/bin/env bash
# Ручная установка через тот же installer, который использует Android.
set -euo pipefail
if [ "${1:-}" = --help ] || [ "$#" -ne 1 ]; then
    printf 'Usage: sudo -E bash scripts/install_server.sh /absolute/path/to/csqtt\n'
    printf 'Required env: CSQTT_MAIN_PASSWORD, CSQTT_WEB_PASS\n'
    printf 'Optional: CSQTT_WEB_USER, CSQTT_TUN_SUBNET, CSQTT_PEER_PORT, CSQTT_WEB_PORT, CSQTT_SSH_PORT, CSQTT_DEPLOY_MODE\n'
    exit 0
fi
[ "$(id -u)" -eq 0 ] || { echo 'Нужны root-права' >&2; exit 1; }
: "${CSQTT_MAIN_PASSWORD:?Укажите пароль туннеля}"
: "${CSQTT_WEB_PASS:?Укажите пароль web-панели}"
command -v python3 >/dev/null || { echo 'Нужен python3' >&2; exit 1; }
binary=$(realpath "$1")
[ -x "$binary" ] || { echo 'Не найден исполняемый server binary' >&2; exit 1; }
"$binary" --version
script_dir=$(cd "$(dirname "$0")" && pwd)
deploy="$script_dir/../app/src/main/assets/deploy.sh"
export CSQTT_WEB_USER="${CSQTT_WEB_USER:-admin}"
export CSQTT_MAIN_PASSWORD CSQTT_WEB_PASS
umask 077
staging=$(mktemp -d /var/tmp/csqtt-manual.XXXXXXXX)
trap 'rm -f "$staging/web.env" "$staging/overrides.json"; rmdir "$staging"' EXIT
python3 - "$staging" <<'PY'
import json, os, pathlib, sys
directory = pathlib.Path(sys.argv[1])
values = {key: os.environ[key] for key in ('CSQTT_WEB_USER', 'CSQTT_WEB_PASS')}
for value in values.values():
    if not value or any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise SystemExit('Недопустимое значение web credentials')
# systemd EnvironmentFile quoting; Docker env-file uses literal values.
def encode(value):
    if os.environ.get('CSQTT_DEPLOY_MODE', 'systemd') == 'docker':
        return value
    return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
(directory / 'web.env').write_text(''.join(f'{key}={encode(value)}\n' for key, value in values.items()))
(directory / 'overrides.json').write_text(json.dumps({
    'main_password': os.environ['CSQTT_MAIN_PASSWORD'], 'device_id': '', 'dns': '1.1.1.1,1.0.0.1'
}))
PY
# Validation happens before prepare stops the old runtime.
# shellcheck disable=SC1090
source <(sed '$d' "$deploy")
configure_tunnel_subnet
bash "$deploy" prepare
install -m 0755 "$binary" /tmp/.csqtt-upload-server
install -m 0600 "$staging/web.env" /tmp/.csqtt-upload-web.env
install -m 0600 "$staging/overrides.json" /tmp/.csqtt-upload-overrides.json
bash "$deploy" install
