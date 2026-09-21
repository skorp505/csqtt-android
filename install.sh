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
DEFAULT_TAG="v2.1.13-r4"
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
    # DISTRIB_ARCH из /etc/openwrt_release — авторитетный повторяемый arch
    # (apk --print-arch может вернуть общий aarch64, игнорируя Cortex-A53 tuning).
    local arch=""
    arch=$(sed -n "s/^DISTRIB_ARCH='\([^']*\)'/\1/p" /etc/openwrt_release 2>/dev/null)
    [ -n "$arch" ] || arch=$(apk --print-arch 2>/dev/null)
    [ -n "$arch" ] || arch=$(opkg print-architecture 2>/dev/null | tail -n1 | awk '{print $2}')
    [ -n "$arch" ] || { err "Не удалось определить архитектуру роутера."; exit 1; }
    echo "$arch"
}

get_client_name() {
    # имя файла клиента для (apk|ipk, arch, version, release-suffix)
    local kind="$1" arch="$2" ver="$3" rel="$4"
    if [ "$kind" = "apk" ]; then
        case "$arch" in
            aarch64_cortex-a53) echo "csqtt-client-${ver}-${rel}_aarch64_cortex-a53.apk" ;;
            aarch64_generic) echo "csqtt-client-${ver}-${rel}.apk" ;;
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
        echo "luci-app-csqtt-${ver}-r4_noarch.apk"
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
    # VER_FULL — полная версия (с суффиксом релиза), VER_NUM — версия бинарника, REL — суффикс
    VER_FULL="$VER"
    VER_NUM=${VER%%-*}
    REL=${VER#*-}
    [ "$REL" = "$VER_NUM" ] && REL="r4"   # тег без суффикса — подставляем актуальный релиз
    msg "Версия релиза: $VER_FULL"

    CLIENT_NAME=$(get_client_name "$kind" "$ARCH" "$VER_NUM" "$REL")
    if [ -z "$CLIENT_NAME" ]; then
        err "Для архитектуры $ARCH сборка пока не опубликована."
        msg "Доступные сборки:"
        msg "  - aarch64_cortex-a53 (Raspberry Pi 3/4 и др., OpenWrt 25.12+)"
        msg "  - aarch64_generic (x86_64 armsr/VM, OpenWrt 24.10/25.12+)"
        msg "Другие архитектуры можно заказать в Issues: https://github.com/$REPO/issues"
        exit 1
    fi
    PANEL_NAME=$(get_panel_name "$VER_NUM")

    if [ -n "$JSON" ]; then
        CLIENT_URL=$(get_url "$JSON" "$CLIENT_NAME")
        [ -n "$CLIENT_URL" ] || warn "Не нашёл ассет в релизе, пробую прямую ссылку."
    fi
    [ -n "${CLIENT_URL:-}" ] || CLIENT_URL="https://github.com/$REPO/releases/download/v${VER_FULL}/${CLIENT_NAME}"
    PANEL_URL="https://github.com/$REPO/releases/download/v${VER_FULL}/${PANEL_NAME}"

    pkg_list_update || { err "Не удалось обновить список пакетов."; exit 1; }

    # Зависимости клиента: клиент использует `iptables -m conntrack`
    # и NAT MASQUERADE для туннеля TUN — их ставят iptables-nft + kmods.
    msg "Устанавливаю зависимости (iptables-nft, kmod-ipt-conntrack, kmod-ipt-nat)..."
    for dep in iptables-nft kmod-ipt-conntrack kmod-ipt-nat; do
        pkg_install "$dep" || warn "Не удалось установить $dep (возможно, уже установлен или недоступен в репозитории)."
    done

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

    # Пост-установочный фикс: всегда ставим актуальный hook туннеля (csqtt-tun).
    # У старых сборок (r3 и ранее) hook не знал про `inet fw4 forward` —
    # пакеты из LAN шли на сброс в firewall OpenWrt 24.10+/fw4.
    msg "Устанавливаю актуальный hook туннеля (csqtt-tun)..."
    mkdir -p /usr/libexec
    HOOK_URL="https://raw.githubusercontent.com/$REPO/main/openwrt/files/usr/libexec/csqtt-tun"
    if fetch "$HOOK_URL" > /usr/libexec/csqtt-tun 2>/dev/null && [ -s /usr/libexec/csqtt-tun ] && grep -q 'inet fw4 forward' /usr/libexec/csqtt-tun; then
        chmod 0755 /usr/libexec/csqtt-tun
        msg "Hook обновлён."
        if [ "$(uci -q get csqtt.main.enabled 2>/dev/null)" = "1" ]; then
            /etc/init.d/csqtt restart 2>/dev/null && msg "Сервис перезапущен с новым hook."
        fi
    else
        warn "Не удалось обновить hook — при включённом туннеле вручную проверьте правило fw4 (VPN -> LAN)!"
    fi

    rm -rf "$DOWNLOAD_DIR"

    cat <<EOF

CSQTT установлен. Настройка — через веб-панель LuCI:
  Службы (Services) -> CSQTT

1. Укажите сервер (peer), пароль и при необходимости VK-хеши.
2. Отметьте «Включить сервис» и нажмите «Сохранить и применить».
3. Нажмите «▶ Старт» и «Показать логи» для проверки работы.

То же самое в консоли:
  vi /etc/config/csqtt
  /etc/init.d/csqtt enable
  /etc/init.d/csqtt restart
  logread -e csqtt

Удаление (конфиг сохранится в /etc/config/csqtt.bak):
  sh <(wget -O - https://raw.githubusercontent.com/skorp505/csqtt-android/main/uninstall.sh)
EOF
}

main