# Ручная установка и API CSQTT

Эти возможности добавлены в `2.1.11`. Для новой подсети и SOCKS5 host нужен
сервер этой версии; опубликованный ранее `v2.1.10` их не содержит.

## Установка без Android

На машине сборки:

```bash
cd rust-server
cargo +1.97.1 test --locked --target x86_64-unknown-linux-gnu
cargo +1.97.1 zigbuild --release --locked --target x86_64-unknown-linux-musl
```

Для ARM-VPS цель `aarch64-unknown-linux-musl` (`arm64`) или
`armv7-unknown-linux-musleabihf` (`armv7`); `rust-server/build_linux.sh --arch all`
собирает все три в `dist/csqtt-linux-<arch>`.

На VPS перенесите бинарник `target/<target>/release/csqtt` нужной архитектуры,
`scripts/install_server.sh` и `app/src/main/assets/deploy.sh`, сохранив
взаимное расположение двух скриптов. VPS должен иметь доступ к пакетным
репозиториям и интернету; для wrapper нужен Python 3. Используется тот же
systemd/Docker installer, что и в Android. Он останавливает существующий CSQTT,
настраивает TUN, forwarding, NAT, firewall и TLS, сохраняя SQLite.

Из каталога с `scripts/`, в root-shell:

```bash
read -rsp 'Пароль туннеля: ' CSQTT_MAIN_PASSWORD; echo
read -rsp 'Пароль web-панели: ' CSQTT_WEB_PASS; echo
export CSQTT_MAIN_PASSWORD CSQTT_WEB_PASS
export CSQTT_WEB_USER=admin
export CSQTT_TUN_SUBNET=10.77.88.0/24
bash scripts/install_server.sh /absolute/path/to/csqtt
unset CSQTT_MAIN_PASSWORD CSQTT_WEB_PASS
```

Выберите сеть, которая не пересекается с LAN, Docker, другими VPN и маршрутами
к серверу/SOCKS5. Поддерживаются частные IPv4-сети `/24`: шлюз `.1`, устройства
`.2`–`.250`. Произвольные маски не поддерживаются текущей таблицей маршрутов.
По умолчанию используется `10.66.67.0/24`.

Дополнительные environment options: `CSQTT_DEPLOY_MODE=systemd|docker`,
`CSQTT_PEER_PORT=46010`, `CSQTT_WEB_PORT=46002`, `CSQTT_SSH_PORT=22`.
Порт SSH должен совпадать с реальным портом SSH на VPS.

Подсеть сохраняется в `/etc/csqtt/csqtt.env` и применяется также к network helper.
Повторный deploy без `CSQTT_TUN_SUBNET` сохраняет прежний выбор. Для смены сети
повторите установку с новым значением: потребуются обновление firewall и
переподключение клиентов. Идентификаторы устройств, ключи и счётчики сохраняются;
IP переносятся в новую сеть с сохранением последнего октета. Одного изменения
environment и рестарта недостаточно для обновления установленных NAT-правил.

После установки проверьте `systemctl status csqtt`, `journalctl -u csqtt`,
адрес `csqtt1`, вход в HTTPS-панель и реальное подключение клиента. Для Docker
используйте `docker logs csqtt` и `docker inspect csqtt`. Wrapper не заменяет
проверку трафика на конкретном VPS.

## DNS и устройства

SOCKS5 CONNECT в 2.1.11 требует клиента и сервера 2.1.11: `CSQPX2` проверяет
offset DATA-фрагментов. Потеря/перестановка завершают stream, автоматической
доставки потерянных фрагментов нет. Обычный IP VPN использует прежний контракт.

В настройках web-панели доступны произвольные IPv4 DNS, включая внутренние.
Сервер сохраняет их в SQLite. При ответе `restart_required: true` выполните
контролируемый рестарт и переподключите клиенты. Desktop использует DNS из
`TUNCONF`, а не фиксированного провайдера.

Один пароль уже поддерживает несколько устройств с разными `device_id` и IP.
В `GET /api/clients` поле `devices` содержит все пары `device_id`/`ip`; панель
показывает этот список при редактировании ключа. Старые `device_id` и `ip`
сохранены для совместимости API. Отвязка, деактивация и удаление действуют на
весь ключ. Одно устройство остаётся привязанным к одному паролю. Общий пул —
249 адресов, независимо от количества ключей.

## SOCKS5 на другом IP

В профиле SOCKS5 задайте `host`, например IPv4 соседнего контейнера в ipvlan,
и `port`. По умолчанию `host=127.0.0.1`; старые JSON/SQLite-профили мигрируют
автоматически. В этой версии принимается числовой unicast IPv4, без hostname
и IPv6. SOCKS5 должен поддерживать TCP и UDP ASSOCIATE; обе проверки используют
указанный host. Маршрут к нему должен быть доступен из server runtime.

## Текущий HTTP API для внешних панелей

API обслуживается тем же HTTPS endpoint, что и web-панель. Сначала выполните
`POST /api/login` с JSON `{"user":"admin","pass":"..."}` и сохраняйте cookie
из `Set-Cookie`. Последующие запросы используют эту cookie. Сессии ограничены
по времени; после `401` нужен повторный вход. Поддерживается также формат
`c1:` из встроенной панели: UTF-8 → прибавить 47 к каждому байту modulo 256 →
Base64. Это кодирование, защиту транспорта обеспечивает TLS.

Используйте доверенный сертификат или явно доверенный CA VPS. Не отключайте
проверку TLS в клиенте API. Credentials и cookies не должны попадать в access
логи, командную строку, репозиторий или общедоступные файлы.

| Метод и путь | Назначение / JSON |
| --- | --- |
| `GET /api/stats` | Состояние сервера, сессии и счётчики |
| `GET /api/settings` | DNS, `tunnel_subnet`, restart status |
| `POST /api/settings` | `{"dns_primary":"192.168.1.1","dns_secondary":""}`; также `main_password`, `auto_restart_interval_hours` |
| `GET /api/clients` | Ключи, сроки, счётчики, массив `devices` |
| `POST /api/clients` | `{"name":"Семья","days":30,"hash":""}`; возвращает созданный пароль |
| `POST /api/clients/{password}` | Обновление параметров ключа; поля как в `CreateClientRequest` |
| `POST /api/clients/{password}/toggle` | Переключение активности и завершение сессий ключа |
| `POST /api/clients/{password}/unbind` | Отвязка всех устройств ключа |
| `DELETE /api/clients/{password}` | Удаление ключа и прекращение доступа |
| `GET /api/local-proxy` | Профили и runtime status |
| `POST /api/local-proxy` | `{"name":"Container","host":"192.168.88.12","port":45000,"username":"","password":""}` |
| `PUT /api/local-proxy/profiles/{id}` | Замена параметров профиля, включая `host` |
| `POST /api/local-proxy/activate/{id}` | Активация профиля |
| `POST /api/local-proxy/deactivate` | Возврат к прямому выходу VPS |
| `DELETE /api/local-proxy/profiles/{id}` | Удаление профиля |
| `POST /api/logout` | Завершение текущей web-сессии |

Пути с password требуют URL-encoding и исключения из access-логов.
Для синхронизации Android есть отдельный `GET /api/client-config/{id}`:
он использует HMAC/HKDF envelope вместо web-cookie и не является admin API.
Не передавайте connection password в URL синхронизации.

Сохранение настроек и применение — разные этапы: проверяйте HTTP status,
`restart_required` и заголовок `x-csqtt-reload-error`. Максимальный JSON body —
16 KiB. Подсеть изменяется через installer/environment с рестартом, не через
`POST /api/settings`. API пока не имеет отдельного versioned prefix; приведён
контракт текущей сборки, исходные handlers находятся в `rust-server/web_panel.rs`.
