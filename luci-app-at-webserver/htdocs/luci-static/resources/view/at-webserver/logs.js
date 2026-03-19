'use strict';
'require view';
'require fs';
'require ui';
'require uci';

return view.extend({
	load: function() {
		return uci.load('at-webserver').then(function() {
			var notifyPersist = uci.get('at-webserver', 'config', 'notify_log_persist') == '1';
			var syslogPersist = uci.get('at-webserver', 'config', 'sys_log_persist') == '1';
			var notifyPath = notifyPersist ? '/var/log/at-notifications.log' : '/tmp/at-notifications.log';
			var sysPath    = syslogPersist  ? '/var/log/at-webserver.log'    : '/tmp/at-webserver.log';

			return Promise.all([
				fs.read_direct(notifyPath).then(function(r) {
					return r.trim() ? r : _('------ 推送通知日志为空 ------');
				}).catch(function() {
					return _('------ 日志文件暂未生成 ------\n(路径: ' + notifyPath + ')');
				}),
				fs.read_direct(sysPath).then(function(r) {
					return r.trim() ? r : _('------ 系统运行日志为空 ------');
				}).catch(function() {
					return _('------ 日志文件暂未生成 ------\n(路径: ' + sysPath + ')');
				})
			]).then(function(results) {
				return {
					notifyData: results[0], notifyPath: notifyPath,
					sysData:    results[1], sysPath:    sysPath
				};
			});
		});
	},

	render: function(info) {
		var splitStyle   = 'display:flex; flex-direction:column; gap:0; height:calc(100vh - 120px); min-height:500px;';
		var paneStyle    = 'flex:1; display:flex; flex-direction:column; min-height:0; overflow:hidden;';
		var dividerStyle = 'height:6px; background:linear-gradient(90deg,#0099CC,#00c8ff,#0099CC); cursor:ns-resize; flex-shrink:0; border-radius:3px; margin:2px 0;';
		var headerStyle  = 'display:flex; align-items:center; justify-content:space-between; padding:6px 10px; background:#1e1e2e; color:#cdd6f4; font-family:monospace; font-size:13px; flex-shrink:0; border-radius:4px 4px 0 0;';
		var taNotify     = 'width:100%; flex:1; font-family:monospace; font-size:12px; resize:none; background:#f8f9fa; color:#333; border:none; padding:8px; box-sizing:border-box; overflow:auto;';
		var taSys        = 'width:100%; flex:1; font-family:monospace; font-size:12px; resize:none; background:#1e1e2e; color:#a6e3a1; border:none; padding:8px; box-sizing:border-box; overflow:auto;';

		// 判断 textarea 是否滚动到底部（误差 4px）
		function isAtBottom(ta) {
			return ta.scrollHeight - ta.scrollTop - ta.clientHeight < 4;
		}

		function makeBtn(label, cls, fn) {
			return E('button', {
				class: 'btn cbi-button ' + cls,
				style: 'margin-left:8px; padding:2px 10px; font-size:12px;',
				click: fn
			}, label);
		}

		var notifyTA = E('textarea', {
			id: 'notify_log_content',
			style: taNotify,
			readonly: true,
			wrap: 'off'
		}, info.notifyData);

		var sysTA = E('textarea', {
			id: 'sys_log_content',
			style: taSys,
			readonly: true,
			wrap: 'off'
		}, info.sysData);

		// 自动刷新：每 3 秒轮询一次，若已在底部则自动滚底，否则保持当前位置
		function startAutoRefresh(ta, path, emptyMsg) {
			return setInterval(function() {
				var atBottom = isAtBottom(ta);
				fs.read_direct(path).then(function(r) {
					var newVal = r.trim() ? r : emptyMsg;
					if (ta.value !== newVal) {
						ta.value = newVal;
						if (atBottom) ta.scrollTop = ta.scrollHeight;
					}
				}).catch(function() {});
			}, 3000);
		}

		// 页面加载后滚到底部，并启动自动刷新
		setTimeout(function() {
			notifyTA.scrollTop = notifyTA.scrollHeight;
			sysTA.scrollTop    = sysTA.scrollHeight;
			startAutoRefresh(notifyTA, info.notifyPath, _('------ 推送通知日志为空 ------'));
			startAutoRefresh(sysTA,    info.sysPath,    _('------ 系统运行日志为空 ------'));
		}, 50);

		// 拖拽分隔条
		var divider = E('div', { style: dividerStyle, title: _('拖拽调整上下比例') });
		var container;
		divider.addEventListener('mousedown', function(e) {
			e.preventDefault();
			var startY = e.clientY;
			var panes  = container.querySelectorAll('.log-pane');
			var topH   = panes[0].getBoundingClientRect().height;
			var botH   = panes[1].getBoundingClientRect().height;
			function onMove(e) {
				var dy = e.clientY - startY;
				panes[0].style.flex = 'none';
				panes[0].style.height = Math.max(80, topH + dy) + 'px';
				panes[1].style.flex = 'none';
				panes[1].style.height = Math.max(80, botH - dy) + 'px';
			}
			function onUp() {
				document.removeEventListener('mousemove', onMove);
				document.removeEventListener('mouseup', onUp);
			}
			document.addEventListener('mousemove', onMove);
			document.addEventListener('mouseup', onUp);
		});

		var notifyPane = E('div', { class: 'log-pane', style: paneStyle }, [
			E('div', { style: headerStyle }, [
				E('span', {}, '\u{1F4E8} ' + _('推送通知日志') + '  \u200B' + E('code', { style: 'font-size:11px; color:#89b4fa;' }, info.notifyPath).outerHTML),
				E('span', {}, [
					makeBtn(_('滚到底部'), 'cbi-button-action', function() {
						notifyTA.scrollTop = notifyTA.scrollHeight;
					}),
					makeBtn(_('清空'), 'cbi-button-negative', function() {
						fs.write(info.notifyPath, '').then(function() {
							notifyTA.value = _('------ 推送通知日志已清空 ------');
							ui.addNotification(null, E('p', _('推送日志已清空')), 'info');
						}).catch(function(e) { ui.addNotification(null, E('p', '清空失败: ' + e.message), 'error'); });
					})
				])
			]),
			notifyTA
		]);

		var sysPane = E('div', { class: 'log-pane', style: paneStyle }, [
			E('div', { style: headerStyle }, [
				E('span', {}, '\u{1F5A5} ' + _('系统运行日志') + '  \u200B' + E('code', { style: 'font-size:11px; color:#89b4fa;' }, info.sysPath).outerHTML),
				E('span', {}, [
					makeBtn(_('滚到底部'), 'cbi-button-action', function() {
						sysTA.scrollTop = sysTA.scrollHeight;
					}),
					makeBtn(_('清空'), 'cbi-button-negative', function() {
						fs.write(info.sysPath, '').then(function() {
							sysTA.value = _('------ 系统运行日志已清空 ------');
							ui.addNotification(null, E('p', _('系统日志已清空')), 'info');
						}).catch(function(e) { ui.addNotification(null, E('p', '清空失败: ' + e.message), 'error'); });
					})
				])
			]),
			sysTA
		]);

		container = E('div', { style: splitStyle }, [ notifyPane, divider, sysPane ]);

		return E('div', { class: 'cbi-map' }, [
			E('h2', {}, _('日志')),
			container
		]);
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
