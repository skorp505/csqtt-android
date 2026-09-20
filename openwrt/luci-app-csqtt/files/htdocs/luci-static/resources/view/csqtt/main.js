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

var REFRESH_MS = 4000; // интервал автообновления статуса и логов

/* ── Стили ───────────────────────────────────────────── */

var STYLES =
    '.csqtt-container{--acc:#8a5cf6;--acc2:#c084fc;}' +
    '.csqtt-status{display:flex;flex-wrap:wrap;align-items:center;gap:10px;' +
        'background:linear-gradient(180deg,rgba(180,140,255,0.07),rgba(180,140,255,0.02));' +
        'border:1px solid rgba(138,92,246,0.22);border-radius:12px;' +
        'padding:16px 18px;margin-bottom:14px;}' +
    '.csqtt-badge{display:inline-block;padding:4px 12px;border-radius:999px;' +
        'font-size:0.8em;font-weight:600;text-transform:uppercase;letter-spacing:0.05em;}' +
    '.csqtt-badge.running{background:rgba(63,185,80,0.16);color:#3fb950;' +
        'border:1px solid rgba(63,185,80,0.4);}' +
    '.csqtt-badge.stopped{background:rgba(139,148,158,0.12);color:#8b949e;' +
        'border:1px solid rgba(139,148,158,0.3);}' +
    '.csqtt-meta{font-size:0.82em;color:#8b949e;}' +
    '.csqtt-actions{margin-left:auto;display:flex;gap:8px;flex-wrap:wrap;}' +
    '.csqtt-logwrap{margin-top:14px;}' +
    '.csqtt-loghead{display:flex;align-items:center;gap:10px;margin-bottom:6px;}' +
    '.csqtt-loghead .csqtt-logtitle{flex:1;font-size:0.85em;color:#8b949e;}';

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

function serviceCommand(command) {
    return callExec('/etc/init.d/csqtt', [ command ]).then(function (res) {
        return res ? res.stdout : '';
    });
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
        var badge = E('span', { class: 'csqtt-badge ' + (status.running ? 'running' : 'stopped') },
            status.running ? 'Работает' : (enabled ? 'Остановлен' : 'Отключён'));

        var meta = E('span', { class: 'csqtt-meta' }, '');
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
        self.updateStatus(badge, meta, status);

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

        var statusCard = E('div', { class: 'csqtt-status' }, [
            badge, meta,
            E('span', { class: 'csqtt-actions' }, [ startBtn, stopBtn, enaBtn, disBtn ])
        ]);

        /* Логи */
        var logBox = E('pre', {
            style: 'background:#0a0518;color:#c4a0ff;padding:10px;min-height:120px;max-height:300px;' +
                   'overflow-y:auto;border-radius:6px;font-size:0.72em;white-space:pre-wrap;' +
                   'word-break:break-all;margin-top:6px;border:1px solid rgba(138,92,246,0.2);display:none;'
        });

        var logTitle = E('span', { class: 'csqtt-logtitle' }, 'Просмотр логов сервиса (logread -e csqtt).');
        function setLogMeta(lines) {
            var stamp = new Date().toLocaleTimeString('ru-RU');
            logTitle.textContent = 'Просмотр логов сервиса (logread -e csqtt).' +
                (lines === -1 ? ' · logread недоступен' : ' · строк: ' + lines + ' · обновлено ' + stamp);
        }

        function refreshLogs() {
            return callExec('/sbin/logread', [ '-e', 'csqtt' ]).then(function (res) {
                var lines = (res || '').split('\n').filter(function (l) { return l.trim() !== ''; }).length;
                logBox.textContent = res || '(записей с тегом csqtt нет)';
                setLogMeta(lines);
            }).catch(function () {
                logBox.textContent = '(logread недоступен — проверьте ACL)';
                setLogMeta(-1);
            });
        }

        var logBoxVisible = false;
        var logBtn = E('button', {
            class: 'btn cbi-button-action',
            click: ui.createHandlerFn(self, function () {
                logBoxVisible = !logBoxVisible;
                logBox.style.display = logBoxVisible ? 'block' : 'none';
                if (logBoxVisible)
                    return refreshLogs();
                return Promise.resolve();
            })
        }, 'Показать логи');

        /* Кнопка выключения сервиса */
        var offBtn = E('button', {
            class: 'btn cbi-button-reset',
            click: ui.createHandlerFn(self, function () {
                return serviceAction('stop').then(function () {
                    ui.hideLoading();
                    ui.addNotification(null, E('p', 'Сервис выключен — готово'), 'info');
                    return getServiceStatus().then(function (s) { self.updateStatus(badge, meta, s); });
                }).catch(function (e) { ui.hideLoading(); ui.addNotification(null, E('p', 'Ошибка: ' + (e.message || e)), 'error'); });
            })
        }, 'Выключить сервис');

        var logWrap = E('div', { class: 'csqtt-logwrap' }, [
            E('div', { class: 'csqtt-loghead' }, [
                logTitle, logBtn, offBtn
            ]),
            logBox
        ]);

        /* Автообновление статуса и логов */
        var timer = null;
        function startAutoRefresh() {
            if (timer)
                return;
            timer = window.setInterval(function () {
                getServiceStatus().then(function (s) {
                    self.updateStatus(badge, meta, s);
                    if (logBoxVisible)
                        refreshLogs();
                }).catch(function () {});
            }, REFRESH_MS);
        }
        startAutoRefresh();

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
            'Адрес CSQTT-сервера в формате host:port.');
        peer.datatype = 'hostport';
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

        var obfs = s.option(form.ListValue, 'obfs', 'Маскировка (obfs)');
        obfs.value('audio', 'audio');
        obfs.value('video', 'video');

        var turn_transport = s.option(form.ListValue, 'turn_transport', 'TURN транспорт');
        turn_transport.value('udp', 'UDP');
        turn_transport.value('tcp', 'TCP (TLS)');

        var socks5_listen = s.option(form.Value, 'socks5_listen', 'SOCKS5 адрес прослушивания');
        socks5_listen.datatype = 'hostport';
        socks5_listen.default = '127.0.0.1:1080';
        socks5_listen.depends('mode', 'socks5');

        var tun_device = s.option(form.Value, 'tun_device', 'TUN-интерфейс');
        tun_device.default = 'csqtt0';
        tun_device.depends('mode', 'tun');

        var lan_device = s.option(form.Value, 'lan_device', 'LAN интерфейс',
            'Трафик этого интерфейса будет идти через туннель.');
        lan_device.default = 'br-lan';
        lan_device.depends('mode', 'tun');

        var route_lan = s.option(form.Flag, 'route_lan', 'Маршрутизировать трафик LAN');
        route_lan.default = '1';
        route_lan.depends('mode', 'tun');

        var route_table = s.option(form.Value, 'route_table', 'Номер таблицы маршрутов');
        route_table.datatype = 'uinteger';
        route_table.default = '202';
        route_table.depends('mode', 'tun');

        return Promise.resolve(m.render()).then(function (formNode) {
            return E('div', { class: 'csqtt-container' }, [
                E('style', STYLES),
                E('h2', {}, 'CSQTT'),
                E('p', { class: 'cbi-section-descr' },
                    'После изменения настроек нажмите «Сохранить и применить» — сервис перезапустится автоматически.'),
                statusCard,
                formNode,
                logWrap,
                E('div', { class: 'csqtt-footer' }, [
                    E('span', { class: 'csqtt-footerver' },
                        'Версия панели: 2.1.13-r3 · ver 25.12')
                ])
            ]);
        });

        /* Очистка таймера при уходе со страницы */
        self.handleCleanup = function () {
            if (timer)
                window.clearInterval(timer);
            timer = null;
        };
        window.addEventListener('pagehide', self.handleCleanup);
    }
});
