#!/bin/sh
# shellcheck shell=dash
#
# CSQTT — удаление OpenWrt-клиента и LuCI-панели.
#
# Одной строкой (на роутере, от root):
#   sh <(wget -O - https://raw.githubusercontent.com/skorp505/csqtt-android/main/uninstall.sh)
#
# Конфигурация /etc/config/csqtt сохраняется как /etc/config/csqtt.bak.

set -eu

PKG_IS_APK=0
command -v apk >/dev/null 2>&1 && PKG_IS_APK=1

msg() { printf '\033[32;1m%s\033[0m\n' "$1"; }
warn() { printf '\033[33;1m%s\033[0m\n' "$1"; }
err() { printf '\033[31;1m%s\033[0m\n' "$1"; }

pkg_remove() {
    # apk del панель и клиент; при ошибках считаем пакет уже отсутствующим
    if [ "$PKG_IS_APK" -eq 1 ]; then
        apk del luci-app-csqtt 2>/dev/null || true
        apk del csqtt-client 2>/dev/null || true
    else
        opkg remove --force-depends luci-app-csqtt 2>/dev/null || true
        opkg remove --force-depends csqtt-client 2>/dev/null || true
    fi
}

main() {
    [ "$(id -u)" -eq 0 ] || { err "Запустите от root: sh <(wget -O - .../uninstall.sh)"; exit 1; }

    msg "Останавливаю сервис CSQTT..."
    if [ -x /etc/init.d/csqtt ]; then
        /etc/init.d/csqtt stop 2>/dev/null || true
        /etc/init.d/csqtt disable 2>/dev/null || true
    fi

    msg "Удаляю пакеты luci-app-csqtt и csqtt-client..."
    pkg_remove

    # Зачистка на случай неполного удаления
    rm -f /usr/bin/csqtt-client /usr/libexec/csqtt-tun /etc/init.d/csqtt

    if [ -f /etc/config/csqtt ] && [ ! -f /etc/config/csqtt.bak ]; then
        mv /etc/config/csqtt /etc/config/csqtt.bak
        msg "Конфигурация сохранена: /etc/config/csqtt.bak"
    fi

    msg "CSQTT удалён. LuCI-страница исчезнет после обновления страницы."
}

main