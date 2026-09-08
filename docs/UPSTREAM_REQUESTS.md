# Запросы upstream и правки нашего fork — 2026-09-08

Источник: [issues amurcanov/csqtt](https://github.com/amurcanov/csqtt/issues).
Ниже состояние исходников релиза `2.1.11`; live deploy выполняется отдельно.

| Запрос | Реализация у нас |
| --- | --- |
| [#18: собственный DNS](https://github.com/amurcanov/csqtt/issues/18) | Уже доступен через панель/API и сохраняется в SQLite; исправлено применение DNS в desktop |
| [#20: подсеть](https://github.com/amurcanov/csqtt/issues/20) | Добавлен `CSQTT_TUN_SUBNET`, частная `/24`, TUN/allocator/routes/NAT и перенос IP устройств |
| [#23: SOCKS5 host](https://github.com/amurcanov/csqtt/issues/23) | Добавлен IPv4 `host` в профиль, панель, API, SQLite, health checks и TCP/UDP SOCKS handshake |
| [#25: несколько устройств на ключ](https://github.com/amurcanov/csqtt/issues/25) | Уже поддерживалось protocol layer; добавлен полный список устройств в API и панель |
| [#17](https://github.com/amurcanov/csqtt/issues/17), [#21: ручная установка/API](https://github.com/amurcanov/csqtt/issues/21) | Добавлен wrapper установки без Android и [runbook/API](SERVER_ADMIN.md) |
| [#10: обновление hashes](https://github.com/amurcanov/csqtt/issues/10) | Уже реализовано в Android: подписанный config sync при старте и каждые 6 часов |
| [#12: proxy mode](https://github.com/amurcanov/csqtt/issues/12) | Уже есть loopback SOCKS5 CONNECT; исправлены потери при переполнении очередей |
| [#5](https://github.com/amurcanov/csqtt/issues/5), [#6: OpenWrt](https://github.com/amurcanov/csqtt/issues/6) | Уже есть отдельный beta-порт; остаются аппаратные проверки |
| [#22: Windows](https://github.com/amurcanov/csqtt/issues/22), Linux из [#15](https://github.com/amurcanov/csqtt/issues/15) | Уже есть отдельный Wails desktop |

## Остальные issues и повторная проверка

Проверены все 24 issues upstream (#13 — PR), открытые и закрытые, и их
комментарии. В наших репозиториях также проверены issues/PR; ниже конкретные
границы выводов, а не обещание исправления любого внешнего отчёта.

| Issue | Результат |
| --- | --- |
| #1: TURN готов, трафика нет | Один автор сообщил об исправлении через список исключений приложений; другие сообщения не содержат воспроизведения. У нас есть раздельная диагностика TURN/server handshake и тесты reconnect/epoch |
| #2: sing-box | Архитектурное предложение, не дефект. Автоматической замены transport в 2.1.11 нет |
| #3: Firefox login loop | У нас cookie Secure зависит от HTTPS-конфигурации, это покрыто тестом; конкретный Firefox mobile сценарий не воспроизведён |
| #4: sudo под zsh и SSH key | У нас `rootCommand` сразу ставит `sudo -S`, глобальной замены нет; отдельный sudo password сохраняется в SSH-key режиме |
| #7: domain/IP routing | Desktop поддерживает исключения доменов/IP/CIDR; на сервере поддержан внешний SOCKS5/xray. Это не новая domain routing функция Android |
| #8: YouTube не работает при allowlist | Без device/network logs причину установить нельзя. Не помечено исправленным; проверены существующие настройки исключений и unit tests |
| #9: Keenetic | Поддержка произвольного Keenetic не заявляется; нужен конкретный hardware/firmware target |
| #11: Hydra Router Neo | Интеграция требует описания интерфейса/протокола Hydra; не реализована |
| #14: обрывы после 2.1.5 | Нет логов и воспроизведения на нашем fork; не объявлено исправленным |
| #15: captcha/mobile | Автор upstream предложил 2.1.9. Наши captcha cancel/auth/recovery проверены тестами; полевой отчёт не воспроизведён |
| #16: регион VPS | Обсуждение ограничений выхода VPS, не программный дефект |
| #22 и наш desktop #2 | Наш #2 содержит `DENIED:protocol_mismatch`. В upstream 2.1.9 GETCONF требует восьмое поле `CSQTT-WIRE-2/3`; у нашего fork семипольный контракт. Добавлено понятное сообщение с требованием совместимого server fork |

Desktop/Android `2.1.11` предназначены для сервера `danusha2345/csqtt-android`.
Совместимость с сервером `amurcanov/csqtt 2.1.9` не заявляется. Само присутствие
Wails-клиента не доказывает устранение пользовательского отчёта #22: нужна
проверка полного трафика на Windows. В релизе поставляются согласованные core,
GUI, Wintun и server artifacts.

Исправления review:

- SOCKS5 DATA не вытесняет предыдущие байты из очереди. Переполнение закрывает
  stream, а переполнение общей transport-очереди останавливает туннель. Это
  устраняет продолжение TCP-потока после потерянного фрагмента из-за очереди;
  не является новым reliable transport поверх UDP.
- SOCKS5 использует `CSQPX2` с offset каждого DATA-фрагмента: пропуск,
  повтор или перестановка закрывают поток до передачи последующего фрагмента.
  SOCKS5 требует клиента и сервера 2.1.11; старый `CSQPX1` не совместим.
- Ошибка установки Windows IPv6 guard прерывает подключение и запускает откат.
- Desktop DNS использует адрес из `TUNCONF` и повторяет усечённый UDP-ответ по TCP.
- Учёт трафика устройства берёт `device_id` текущей сессии: трафик остальных
  устройств общего ключа больше не зачисляется первому устройству.

[#19](https://github.com/amurcanov/csqtt/issues/19) про обрыв через шесть секунд
относится к upstream `2.1.9`: воспроизведения на нашем fork нет, исправленным
не объявляется. [#24](https://github.com/amurcanov/csqtt/issues/24) про
`shared/flow_frame.rs`/окно 12 ms неприменим напрямую: такого пересборщика в
текущем fork нет. Переносить константу без соответствующего протокола нельзя.

Запросы интеграции sing-box, Hydra и Keenetic требуют отдельного определения
целевой платформы/протокола. Они не входят в перечисленные изменения и не
помечены реализованными. Ответы и изменения в upstream issues не публиковались.

## Проверки исходников

- Rust client: 260 passed, 4 ignored; Clippy с `-D warnings`.
- Rust server: 117 passed на `x86_64-unknown-linux-gnu`; Clippy с `-D warnings`.
- Android: 159 unit tests, 0 failures/errors; `lintDebug` успешен.
- Desktop: Go tests/race/vet и Windows cross-compilation успешны.
- ShellCheck, тест валидации подсети и синтаксис JavaScript панели успешны.

Live deploy, Windows runtime, физический OpenWrt и полный VK TURN E2E в этой
итерации не выполнялись. Для релиза `desktop/bin/` пересобирается из тех же исходников.
