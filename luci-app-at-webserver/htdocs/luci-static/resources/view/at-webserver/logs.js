'use strict';
'require view';
'require fs';
'require ui';
'require uci';

return view.extend({
	load: function() {
		return uci.load('at-webserver').then(function() {
			var logFile = uci.get('at-webserver', 'config', 'log_file') || '';
			if (!logFile)
				return { path: '', content: '', status: 'unconfigured' };
			
			return fs.stat(logFile).then(function(st) {
				return fs.read(logFile).then(function(content) {
					var lines = (content || '').trim().split('\n');
					if (lines.length > 300)
						lines = lines.slice(lines.length - 300);
					return { path: logFile, content: lines.join('\n'), status: 'ok' };
				});
			}).catch(function(err) {
				// 兼容文件不存在的情况
				var errMsg = String(err);
				if (err && (err.name === 'NotFoundError' || errMsg.indexOf('ENOENT') !== -1 || errMsg.indexOf('No such file') !== -1)) {
					return { path: logFile, content: '', status: 'ok' };
				}
				return { path: logFile, content: '', status: 'error', error: err && err.message ? err.message : errMsg };
			});
		});
	},

	handleRefreshLog: function(ev) {
		window.location.reload();
	},

	handleClearLog: function(ev) {
		return L.Request.get('/cgi-bin/at-log-clear').then(function(res) {
			try {
				var result = res.json();
				if (result.success) {
					ui.addNotification(null, E('p', _('✓ 通知日志已清空')), 'success');
					setTimeout(function() { window.location.reload(); }, 600);
				} else {
					ui.addNotification(null, E('p', _('清空失败: %s').format(result.error || '未知错误')), 'error');
				}
			} catch(e) {
				ui.addNotification(null, E('p', _('清空失败: 解析响应出错')), 'error');
			}
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('清空失败: %s').format(err.message || '请求失败')), 'error');
		});
	},

	render: function(data) {
		var notif = data || { path: '', content: '', status: 'unconfigured' };
		var notifStyle = 'background:#1e1e1e; color:#e6e6e6; border:1px solid #333; padding:15px; border-radius:4px; max-height:400px; overflow-y:auto; font-family:monospace; font-size:13px; line-height:1.6; white-space: pre-wrap; word-wrap: break-word;';
		
		var view = E('div', { 'class': 'cbi-map' }, [
			E('h2', {}, _('日志查看'))
		]);

		// ==========================
		// 第一部分：通知日志模块
		// ==========================
		var notifSection = E('div', { 'class': 'cbi-section' }, [
			E('h3', {}, _('推送通知日志')),
			E('div', { 'class': 'cbi-section-descr' }, _('查看短信、来电、信号变化等历史通知记录'))
		]);

		if (notif.status === 'unconfigured') {
			notifSection.appendChild(
				E('div', { 'class': 'alert-message warning' }, [
					E('h4', {}, _('未配置通知日志文件')),
					E('p', {}, _('请先在配置页面中设置日志文件路径。')),
					E('p', {}, [ E('a', { 'href': L.url('admin/services/at-webserver/config'), 'class': 'btn cbi-button-apply' }, _('前往配置')) ])
				])
			);
		} else if (notif.status === 'error') {
			notifSection.appendChild(
				E('div', { 'class': 'alert-message error' }, [
					E('h4', {}, _('读取日志失败')),
					E('p', {}, _('错误信息: %s').format(notif.error || '未知错误')),
					E('p', {}, _('日志路径: %s').format(notif.path))
				])
			);
		} else if (!notif.content || !notif.content.trim()) {
			notifSection.appendChild(
				E('div', { 'class': 'alert-message info', 'style': 'margin-top: 10px;' }, [
					E('h4', {}, _('日志文件为空')),
					E('p', {}, _('暂无通知记录。日志路径: %s').format(notif.path))
				])
			);
		} else {
			var nlines = notif.content.trim().split('\n').reverse();
			notifSection.appendChild(
				E('div', { 'class': 'cbi-section-node' }, [
					E('div', { 'class': 'cbi-value' }, [
						E('label', { 'class': 'cbi-value-title' }, _('操作')),
						E('div', { 'class': 'cbi-value-field' }, [
							E('button', { 'class': 'btn cbi-button-apply', 'click': ui.createHandlerFn(this, 'handleRefreshLog') }, _('刷新日志')),
							' ',
							E('button', { 'class': 'btn cbi-button-reset', 'click': ui.createHandlerFn(this, 'handleClearLog') }, _('清空通知日志'))
						])
					]),
					E('div', { 'style': 'margin-top: 10px;' }, [ E('pre', { 'style': notifStyle }, nlines.join('\n')) ])
				])
			);
		}
		view.appendChild(notifSection);

		// ==========================
		// 第二部分：系统运行日志模块 (实时抓取)
		// ==========================
		var sysLogSection = E('div', { 'class': 'cbi-section', 'style': 'margin-top: 30px;' }, [
			E('h3', {}, _('系统运行日志')),
			E('div', { 'class': 'cbi-section-descr' }, _('查看 AT WebServer 插件的底层运行状态，日志每 3 秒自动更新。'))
		]);

		// 系统日志专属样式：黑色背景，绿色字体（极客终端风）
		var sysLogPre = E('pre', {
			'style': 'background:#000000; color:#00ff00; border:1px solid #333; padding:15px; border-radius:4px; height:500px; overflow-y:auto; font-family:monospace; font-size:13px; line-height:1.6; white-space: pre-wrap; word-wrap: break-word;'
		}, _('等待获取系统日志...'));

		var isAutoScroll = true;
		var btnAutoScroll = E('button', {
			'class': 'btn cbi-button-apply',
			'click': function(ev) {
				isAutoScroll = !isAutoScroll;
				if (isAutoScroll) {
					ev.target.textContent = _('暂停滚动');
					ev.target.classList.remove('cbi-button-reset');
					ev.target.classList.add('cbi-button-apply');
					sysLogPre.scrollTop = sysLogPre.scrollHeight;
				} else {
					ev.target.textContent = _('恢复滚动');
					ev.target.classList.remove('cbi-button-apply');
					ev.target.classList.add('cbi-button-reset');
				}
			}
		}, _('暂停滚动'));

		// 极其人性化：当鼠标在日志框内向上滚动时，自动暂停页面滚动
		sysLogPre.addEventListener('wheel', function(ev) {
			if (ev.deltaY < 0 && isAutoScroll) {
				btnAutoScroll.click(); // 模拟点击切换为暂停状态
			}
		});

		sysLogSection.appendChild(
			E('div', { 'class': 'cbi-section-node' }, [
				E('div', { 'class': 'cbi-value' }, [
					E('label', { 'class': 'cbi-value-title' }, _('滚动控制')),
					E('div', { 'class': 'cbi-value-field' }, [ btnAutoScroll ])
				]),
				E('div', { 'style': 'margin-top:10px;' }, [ sysLogPre ])
			])
		);
		
		// 轮询抓取底层 logread
		var updateSysLog = function(forceScroll) {
			// -e at-webserver 提取所有带有该标签的日志
			return fs.exec_direct('/sbin/logread', ['-e', 'at-webserver']).then(function(res) {
				if (res && res.trim()) {
					var lines = res.trim().split('\n');
					if (lines.length > 500) {
						lines = lines.slice(lines.length - 500); // 性能保护：只显示最后 500 行
					}
					sysLogPre.textContent = lines.join('\n');
				} else {
					sysLogPre.textContent = _('当前系统缓存中暂无 at-webserver 相关的运行日志。');
				}

				if (isAutoScroll || forceScroll) {
					// 给浏览器渲染留一点时间缓冲再滚动到底部
					setTimeout(function() {
						sysLogPre.scrollTop = sysLogPre.scrollHeight;
					}, 50);
				}
			}).catch(function(err) {
				sysLogPre.textContent = _('抓取系统日志失败，请确保 RPCD 权限已生效。') + '\n' + String(err);
			});
		};

		// 初始加载系统日志
		updateSysLog(true);

		// 加入 LuCI 全局定时器，每 3 秒刷新一次系统日志
		L.Poll.add(updateSysLog, 3);

		view.appendChild(sysLogSection);

		return view;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});