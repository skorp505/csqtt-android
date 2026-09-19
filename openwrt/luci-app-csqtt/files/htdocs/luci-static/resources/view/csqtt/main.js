'use strict';
'require view';
'require rpc';
'require uci';
'require form';
'require ui';

/*
 * luci-app-csqtt — веб-панель CSQTT-клиента для OpenWrt
 * Управляет /etc/config/csqtt (секция main) и сервисом /etc/init.d/csqtt.
 */

/* ── RPC ─────────────────────────────────────────────── */

var callServiceList = rpc.declare({
    object : 'service',
    method : 'list',
    params : [ 'name' ],
    expect : { '': {} }
});

var callInitAction = rpc.declare({
    object : 'rc',
    method : 'init',
    params : [ 'name', 'action' ],
    expect : { result: 0 }
});

var callExec = rpc.declare({
    object : 'file',
    method : 'exec',
    params : [ 'command', 'params' ],
    expect : { stdout: '' }
});

/* ── Стили ───────────────────────────────────────────── */

var STYLES =
    '.csqtt-container{--acc:#8a5cf6;--acc2:#c084fc;}' +
    '.csqtt-status{display:flex;flex-wrap:wrap;align-items:center;gap:10px;' +
        'background:linear-gradient(180deg,rgba(180,140,255,0.07),rgba(180,140,255,0.02));' +
        'border:1px solid rgba(138,92,246,0.22);border-radius:12px;' +
        'padding:16px 18px;margin-bottom:14px;}' +
    '.csqtt-badge{padding:4px 12px;border-radius:999px;font-size:0.8em;font-weight:600;' +
        'text-transform:uppercase;letter-spacing:0.05em;}' +
    '.csqtt-badge.running{background:rgba(63,185,80,0.16);color:#3fb950;' +
        'border:1px solid rgba(63,185,80,0.4);}' +
    '.csqtt-badge.stopped{background:rgba(139,148,158,0.12);color:#8b949e;' +
        'border:1px solid rgba(139,148,158,0.3);}' +
    '.csqtt-meta{font-size:0.82em;color:#8b949e;}' +
    '.csqtt-actions{margin-left:auto;display:flex;gap:8px;flex-wrap:wrap;}';

/* ── Сервис ──────────────────────────────────────────── */

function getServiceStatus() {
    return callServiceList('csqtt').then(function (res) {
        var instances = (res && res.csqtt && res.csqtt.instances) ? res.csqtt.instances : {};
        var running = false, pid = null;
        Object.keys(instances).forEach(function (k) {
            if (instances[k].running) { running = true; pid = instances[k].pid || null; }
        });
        return { running: running, pid: pid };
    }).catch(function () { return { running: false, pid: null }; });
}

function serviceAction(action) {
    return callInitAction('csqtt', action);
}

/* ── View ────────────────────────────────────────────── */

return L.view.extend({
    load: function () {
        return Promise.all([ uci.load('csqtt'), getServiceStatus() ]);
    },

    render: function (data) {
        var self = this;
        var status = data[1];
        var enabled = uci.get('csqtt', 'main', 'enabled') === '1';

        /* Статус и кнопки управления */
        var badge = E('span', {
            class: 'csqtt-badge ' + (status.running ? 'running' : 'stopped')
        }, status.running ? 'Работает' : (enabled ? 'Остановлен' : 'Отключён'));

        var metaParts = [
            E('strong', {}, enabled ? 'Автозапуск: включён' : 'Автозапуск: выключен')
        ];
        if (status.pid)
            metaParts.push(E('span', {}, ' · PID ' + status.pid));
        metaParts.push(E('span', {}, ' · Режим: ' + (uci.get('csqtt', 'main', 'mode') || 'tun')));

        var meta = E('span', { class: 'csqtt-meta' }, metaParts);

        function makeBtn(text, cls, handler) {
            return E('button', {
                class: 'btn cbi-button ' + cls,
                click: ui.createHandlerFn(self, function () {
                    return Promise.resolve()
                        .then(handler)
                        .then(function () {
                            ui.hideLoading();
                            ui.addNotification(null, E('p', text + ' — готово'), 'info');
                            return getServiceStatus().then(function (s) { self.updateStatus(badge, meta, s); });
                        })
                        .catch(function (e) { ui.hideLoading(); ui.addNotification(null, E('p', 'Ошибка: ' + (e.message || e)), 'error'); });
                })
            }, text);
        }

        var startBtn = makeBtn('▶ Старт', 'cbi-button-apply', function () { return serviceAction('start'); });
        var stopBtn  = makeBtn('■ Стоп', 'cbi-button-reset',  function () { return serviceAction('stop'); });
        var enaBtn   = makeBtn('Включить автозапуск', 'cbi-button-save', function () { return serviceAction('enable'); });
        var disBtn   = makeBtn('Отключить автозапуск', 'cbi-button-reset', function () { return serviceAction('disable'); });

        self.updateStatus = function (badgeEl, metaEl, s) {
            var en = uci.get('csqtt', 'main', 'enabled') === '1';
            badgeEl.className = 'csqtt-badge ' + (s.running ? 'running' : 'stopped');
            badgeEl.textContent = s.running ? 'Работает' : (en ? 'Остановлен' : 'Отключён');
            metaEl.textContent = '';
            metaEl.appendChild(E('strong', {}, en ? 'Автозапуск: включён' : 'Автозапуск: выключен'));
            if (s.pid)
                metaEl.appendChild(E('span', {}, ' · PID ' + s.pid));
            metaEl.appendChild(E('span', {}, ' · Режим: ' + (uci.get('csqtt', 'main', 'mode') || 'tun')));
        };

        /* Логи */
        var logBox = E('pre', {
            style: 'background:#0a0518;color:#c4a0ff;padding:10px;min-height:200px;max-height:300px;' +
                   'overflow-y:auto;border-radius:6px;font-size:0.75em;white-space:pre-wrap;' +
                   'word-break:break-all;margin-top:6px;border:1px solid rgba(138,92,246,0.2);display:none;'
        });

        var logBtn = E('button', {
            class: 'btn cbi-button-action',
            click: ui.createHandlerFn(self, function () {
                return callExec('/sbin/logread', [ '-e', 'csqtt' ]).then(function (res) {
                    logBox.style.display = logBox.style.display === 'none' ? 'block' : 'none';
                    if (logBox.style.display === 'block')
                        logBox.textContent = res || '(записей с тегом csqtt нет)';
                }).catch(function () {
                    logBox.style.display = 'block';
                    logBox.textContent = '(logread недоступен — проверьте ACL)';
                });
            })
        }, 'Показать логи');

        var statusCard = E('div', { class: 'csqtt-status' }, [
            badge, meta,
            E('span', { class: 'csqtt-actions' }, [ startBtn, stopBtn, enaBtn, disBtn ])
        ]);

        /* Форма настроек */
        var m = new L.form.Map('csqtt', 'CSQTT — клиент VPN-туннеля',
            'Клиент создаёт системный VPN и передаёт трафик роутера/LAN через TURN.');

        var s = m.section(form.NamedSection, 'main', 'csqtt',
            'Основные настройки');

        var enabledFlag = s.option(form.Flag, 'enabled', 'Включить сервис',
            'Сервис стартует автоматически при загрузке роутера (init.d enable).');

        var mode = s.option(form.ListValue, 'mode', 'Режим работы',
            'TUN — трафик с lan_device уходит в туннель. SOCKS5 — локальный прокси без изменения маршрутов.');
        mode.value('tun', 'TUN (интернет в LAN)');
        mode.value('socks5', 'SOCKS5 (локальный прокси)');

        var peer = s.option(form.Value, 'peer', 'Сервер (peer)',
            'Адрес CSQTT-сервера на VPS в формате host:port.');
        peer.placeholder = 'vpn.example.org:46010';
        peer.mandatory = true;

        var password = s.option(form.Value, 'password', 'Пароль подключения',
            'Пароль, заданный на сервере. При старте переносится в credentials.json (0600).');
        password.password = true;
        password.mandatory = true;

        var vk_hashes = s.option(form.Value, 'vk_hashes', 'VK-хеши',
            'Допустимые хеши VK-звонков через запятую.');
        vk_hashes.placeholder = 'hash1,hash2';
        vk_hashes.mandatory = true;

        var device_id = s.option(form.Value, 'device_id', 'Имя устройства',
            'Идентификатор роутера, видимый на сервере.');
        device_id.datatype = 'string';
        device_id.default = 'openwrt-router';

        var workers = s.option(form.Value, 'workers', 'Потоков (workers)');
        workers.datatype = 'uinteger';
        workers.default = '18';

        var obfs = s.option(form.ListValue, 'obfs', 'Маскировка (obfs)',
            'Тип RTP-потока, которым маскируется трафик.');
        obfs.value('audio', 'audio');
        obfs.value('video', 'video');

        var turnTransport = s.option(form.ListValue, 'turn_transport', 'TURN транспорт');
        turnTransport.value('udp', 'UDP');
        turnTransport.value('tcp', 'TCP (TLS)');

        var turn_host = s.option(form.Value, 'turn_host', 'TURN хост',
            'Обычно пусто — адрес берётся из peer.');
        var turn_port = s.option(form.Value, 'turn_port', 'TURN порт');
        turn_port.datatype = 'port';

        var tun_device = s.option(form.Value, 'tun_device', 'TUN-интерфейс');
        tun_device.default = 'csqtt0';
        tun_device.depends('mode', 'tun');

        var lan_device = s.option(form.Value, 'lan_device', 'LAN интерфейс',
            'Трафик этого интерфейса будет идти через туннель.');
        lan_device.default = 'br-lan';
        lan_device.depends('mode', 'tun');

        var route_lan = s.option(form.Flag, 'route_lan', 'Маршрутизировать трафик LAN',
            'Перехват трафика с lan_device в отдельную таблицу маршрутов.');
        route_lan.default = '1';
        route_lan.depends('mode', 'tun');

        var route_table = s.option(form.Value, 'route_table', 'Номер таблицы маршрутов');
        route_table.datatype = 'uinteger';
        route_table.default = '202';
        route_table.depends('mode', 'tun');

        var socks5_listen = s.option(form.Value, 'socks5_listen', 'SOCKS5 адрес прослушивания',
            'По умолчанию локальный 127.0.0.1:1080. Открывать на LAN — только с firewall-ограничениями.');
        socks5_listen.default = '127.0.0.1:1080';
        socks5_listen.depends('mode', 'socks5');

        return Promise.resolve(m.render()).then(function (formNode) {
            return E('div', { class: 'csqtt-container' }, [
                E('style', STYLES),
                E('h2', {}, 'CSQTT'),
                E('p', { class: 'cbi-section-descr' },
                    'После изменения настроек нажмите «Сохранить и применить» — сервис перезапустится автоматически.'),
                statusCard,
                formNode,
                E('div', { class: 'cbi-section', style: 'margin-top:14px' }, [
                    E('div', { class: 'cbi-section-node' }, [
                        E('div', { class: 'cbi-section-node-content' }, [
                            E('div', { style: 'display:flex;align-items:center;gap:10px;' }, [
                                E('div', { class: 'cbi-section-descr', style: 'margin:0;flex:1;' },
                                    'Просмотр логов сервиса (logread -e csqtt).'),
                                logBtn
                            ]),
                            logBox
                        ])
                    ])
                ])
            ]);
        });
    }
});