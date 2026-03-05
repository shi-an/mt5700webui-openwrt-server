'use strict';
'require view';
'require fs';
'require ui';
'require uci';

return view.extend({
	load: function() {
		return uci.load('at-webserver').then(function() {
			// 智能判断：系统日志的持久化开关
			var isPersist = uci.get('at-webserver', 'config', 'sys_log_persist') == '1';
			var logPath = isPersist ? '/var/log/at-webserver.log' : '/tmp/at-webserver.log';

			return fs.read_direct(logPath).then(function(res) {
				return res.trim() ? res : _('------ 系统运行日志为空 ------');
			}).catch(function(err) {
				return _('------ 系统日志文件暂未生成 ------\n(当前监听路径: ' + logPath + ')');
			}).then(function(logData) {
				return { data: logData, path: logPath };
			});
		});
	},

	render: function(logInfo) {
		return E('div', { class: 'cbi-map' }, [
			E('h2', {}, _('系统运行日志')),
			E('div', { class: 'cbi-map-descr' }, _('查看 AT WebServer 后台守护进程的运行状态。当前日志存储在：') + '<code>' + logInfo.path + '</code>'),
			
			E('div', { class: 'cbi-section' }, [
				E('textarea', {
					id: 'sys_log_content',
					class: 'cbi-input-textarea',
					style: 'width: 100%; height: 500px; font-family: monospace; resize: vertical; background: #272822; color: #f8f8f2;',
					readonly: true,
					wrap: 'off'
				}, logInfo.data)
			]),

			E('div', { class: 'right' }, [
				E('button', {
					class: 'btn cbi-button cbi-button-negative',
					click: function() {
						fs.write(logInfo.path, '').then(function() {
							document.getElementById('sys_log_content').value = _('------ 系统运行日志已清空 ------');
							ui.addNotification(null, E('p', _('日志清空成功！')), 'info');
						}).catch(function(e) {
							ui.addNotification(null, E('p', _('清空失败: ') + e.message), 'error');
						});
					}
				}, _('清空日志'))
			])
		]);
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});