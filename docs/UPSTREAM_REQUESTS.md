# Запросы upstream и правки нашего fork — 2026-09-08

Источник: [issues amurcanov/csqtt](https://github.com/amurcanov/csqtt/issues).
Ниже состояние исходников; публикация нового релиза и deploy ещё не выполнены.

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

Исправления review:

- SOCKS5 DATA не вытесняет предыдущие байты из очереди. Переполнение закрывает
  stream, а переполнение общей transport-очереди останавливает туннель. Это
  устраняет продолжение TCP-потока после потерянного фрагмента из-за очереди;
  не является новым reliable transport поверх UDP.
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

- Rust client: 256 passed, 4 ignored; Clippy с `-D warnings`.
- Rust server: 114 passed на `x86_64-unknown-linux-gnu`; Clippy с `-D warnings`.
- Android: 159 unit tests, 0 failures/errors; `lintDebug` успешен.
- Desktop: Go tests/race/vet и Windows cross-compilation успешны.
- ShellCheck, тест валидации подсети и синтаксис JavaScript панели успешны.

Live deploy, Windows runtime, физический OpenWrt и полный VK TURN E2E в этой
итерации не выполнялись. Ранее существовавшие файлы `desktop/bin/` не заменялись.
