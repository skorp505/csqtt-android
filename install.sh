#!/bin/sh
# shellcheck shell=dash
#
# CSQTT — установка OpenWrt-клиента и LuCI-панели с GitHub Releases.
#
# Одной строкой (на роутере, от root):
#   sh <(wget -O - https://raw.githubusercontent.com/skorp505/csqtt-android/main/install.sh)
#
# Определяет пакетный менеджер (apk/opkg) и архитектуру роутера, скачивает
# подходящие .apk/.ipk из последнего релиза и ставит их вместе с зависимостями.

set -eu

REPO="skorp505/csqtt-android"
REPO_API="https://api.github.com/repos/$REPO/releases/latest"
# Запасной тег на случай недоступности GitHub API (rate limit / сеть)
DEFAULT_TAG="v2.1.13"
DOWNLOAD_DIR="/tmp/csqtt"
COUNT=3

PKG_IS_APK=0
command -v apk >/dev/null 2>&1 && PKG_IS_APK=1

msg() { printf '\033[32;1m%s\033[0m\n' "$1"; }
warn() { printf '\033[33;1m%s\033[0m\n' "$1"; }
err() { printf '\033[31;1m%s\033[0m\n' "$1"; }

fetch() {
    # fetch url -> stdout (wget или curl)
    if command -v wget >/dev/null 2>&1; then
        wget -qO- -T 30 "$1"
    elif command -v curl >/dev/null 2>&1; then
        curl -fsSL --max-time 30 "$1"
    else
        err "Не найден wget или curl для загрузки файлов."
        exit 1
    fi
}

download() {
    # download url dest (с повторами)
    local url="$1" dest="$2" attempt=0
    while [ "$attempt" -lt "$COUNT" ]; do
        attempt=$((attempt + 1))
        msg "Скачиваю $(basename "$dest") (попытка $attempt из $COUNT)..."
        if command -v wget >/dev/null 2>&1; then
            wget -q -O "$dest" -T 60 "$url"
        else
            curl -fsSL --max-time 60 -o "$dest" "$url"
        fi
        if [ -s "$dest" ]; then
            return 0
        fi
        warn "Ошибка загрузки. Повторяю..."
        rm -f "$dest"
    done
    err "Не удалось скачать $(basename "$dest") после $COUNT попыток."
    exit 1
}

pkg_list_update() {
    if [ "$PKG_IS_APK" -eq 1 ]; then
        apk update
    else
        opkg update
    fi
}

pkg_install() {
    if [ "$PKG_IS_APK" -eq 1 ]; then
        apk add --allow-untrusted "$1"
    else
        opkg install "$1"
    fi
}

pkg_is_installed() {
    if [ "$PKG_IS_APK" -eq 1 ]; then
        apk list --installed 2>/dev/null | grep -q "^$1"
    else
        opkg list-installed 2>/dev/null | grep -q "^$1"
    fi
}

get_arch() {
    local arch=""
    if [ "$PKG_IS_APK" -eq 1 ]; then
        arch=$(apk --print-arch 2>/dev/null)
    fi
    [ -n "$arch" ] || arch=$(sed -n "s/^DISTRIB_ARCH='\([^']*\)'/\1/p" /etc/openwrt_release 2>/dev/null)
    [ -n "$arch" ] || arch=$(opkg print-architecture 2>/dev/null | tail -n1 | awk '{print $2}')
    [ -n "$arch" ] || { err "Не удалось определить архитектуру роутера."; exit 1; }
    echo "$arch"
}

get_client_name() {
    # имя файла клиента для (apk|ipk, arch, version)
    local kind="$1" arch="$2" ver="$3"
    if [ "$kind" = "apk" ]; then
        case "$arch" in
            aarch64_cortex-a53) echo "csqtt-client-${ver}-r1_aarch64_cortex-a53.apk" ;;
            aarch64_generic) echo "csqtt-client-${ver}-r1.apk" ;;
            *) echo "" ;;
        esac
    else
        case "$arch" in
            aarch64_generic) echo "csqtt-client_${ver}_aarch64_generic.ipk" ;;
            *) echo "" ;;
        esac
    fi
}

get_panel_name() {
    local ver="$1"
    if [ "$PKG_IS_APK" -eq 1 ]; then
        echo "luci-app-csqtt-${ver}-r1_noarch.apk"
    else
        echo "luci-app-csqtt_${ver}_all.ipk"
    fi
}

get_url() {
    # url по имени файла из тела JSON релиза
    printf '%s' "$1" | grep -o "https://[^\"[:space:]]*$2" | head -n1
}

main() {
    [ "$(id -u)" -eq 0 ] || { err "Запустите установку от root: sh <(wget -O - .../install.sh)"; exit 1; }

    MODEL=$(cat /tmp/sysinfo/model 2>/dev/null || uname -m)
    msg "Роутер: $MODEL"

    ARCH=$(get_arch)
    if [ "$PKG_IS_APK" -eq 1 ]; then
        msg "Архитектура: $ARCH (apk, OpenWrt 25.12+)"
    else
        msg "Архитектура: $ARCH (opkg, OpenWrt 24.10)"
    fi

    # Синхронизируем время — требуется для HTTPS к GitHub
    /usr/sbin/ntpd -q -p 216.239.35.0 -p 162.159.200.1 >/dev/null 2>&1 || true

    kind=$( [ "$PKG_IS_APK" -eq 1 ] && echo apk || echo ipk )

    # Версия из API последнего релиза (с запасным тегом)
    JSON=""
    msg "Получаю информацию о последнем релизе..."
    API_OUT=$(fetch "$REPO_API" 2>/dev/null || true)
    if printf '%s' "$API_OUT" | grep -q 'API rate limit '; then
        warn "GitHub API ограничен (rate limit). Использую запасной тег $DEFAULT_TAG."
    else
        JSON="$API_OUT"
    fi
    VER=$(printf '%s' "$JSON" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
    VER=${VER#v}
    [ -n "$VER" ] || VER=${DEFAULT_TAG#v}
    [ -n "$VER" ] || { err "Не удалось определить версию релиза."; exit 1; }
    msg "Версия релиза: $VER"

    CLIENT_NAME=$(get_client_name "$kind" "$ARCH" "$VER")
    if [ -z "$CLIENT_NAME" ]; then
        err "Для архитектуры $ARCH сборка пока не опубликована."
        msg "Доступные сборки:"
        msg "  - aarch64_cortex-a53 (Raspberry Pi 3/4 и др., OpenWrt 25.12+)"
        msg "  - aarch64_generic (x86_64 armsr/VM, OpenWrt 24.10/25.12+)"
        msg "Другие архитектуры можно заказать в Issues: https://github.com/$REPO/issues"
        exit 1
    fi
    PANEL_NAME=$(get_panel_name "$VER")

    if [ -n "$JSON" ]; then
        CLIENT_URL=$(get_url "$JSON" "$CLIENT_NAME")
        [ -n "$CLIENT_URL" ] || warn "Не нашёл ассет в релизе, пробую прямую ссылку."
    fi
    [ -n "${CLIENT_URL:-}" ] || CLIENT_URL="https://github.com/$REPO/releases/download/v${VER}/${CLIENT_NAME}"
    PANEL_URL="https://github.com/$REPO/releases/download/v${VER}/${PANEL_NAME}"

    pkg_list_update || { err "Не удалось обновить список пакетов."; exit 1; }

    mkdir -p "$DOWNLOAD_DIR"
    download "$CLIENT_URL" "$DOWNLOAD_DIR/$CLIENT_NAME"
    if pkg_is_installed "csqtt-client"; then
        msg "Обнаружен установленный csqtt-client. Обновляю..."
    else
        msg "Устанавливаю csqtt-client..."
    fi
    pkg_install "$DOWNLOAD_DIR/$CLIENT_NAME"

    download "$PANEL_URL" "$DOWNLOAD_DIR/$PANEL_NAME"
    if pkg_is_installed "luci-app-csqtt"; then
        msg "Обнаружена установленная luci-app-csqtt. Обновляю..."
    else
        msg "Устанавливаю luci-app-csqtt..."
    fi
    pkg_install "$DOWNLOAD_DIR/$PANEL_NAME"

    rm -rf "$DOWNLOAD_DIR"

    cat <<EOF

CSQTT установлен. Дальнейшие шаги:

1. Настройте конфигурацию:
   vi /etc/config/csqtt

2. Запустите сервис (с автозапуском):
   /etc/init.d/csqtt enable
   /etc/init.d/csqtt restart

3. Проверьте работу:
   logread -e csqtt

Веб-панель: LuCI -> Службы (Services) -> CSQTT
EOF
}

main