'use strict';
'require view';
'require uci';
'require ui';
'require rpc';

var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: ['name'],
	expect: { '': {} }
});

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('at-webserver'),
			callServiceList('at-webserver')
		]);
	},

	render: function(data) {
		var isRunning = false;
		try {
			isRunning = data[1]['at-webserver']['instances']['instance1']['running'];
		} catch(e) { }

		var wsPort = uci.get('at-webserver', 'config', 'websocket_port') || '8765';
		var wsAuthKey = uci.get('at-webserver', 'config', 'websocket_auth_key') || '';
		var wsUrl = 'ws://' + window.location.hostname + ':' + wsPort;
		
		// 安全处理：转义 JavaScript 字符串中的特殊字符
		var wsAuthKeyEscaped = wsAuthKey
			.replace(/\\/g, '\\\\')   // 反斜杠
			.replace(/'/g, "\\'")      // 单引号
			.replace(/"/g, '\\"')      // 双引号
			.replace(/\n/g, '\\n')     // 换行
			.replace(/\r/g, '\\r')     // 回车
			.replace(/\t/g, '\\t');    // 制表符

		var view = E('div', { 'class': 'cbi-map' }, [
			E('h2', {}, _('系统日志')),
			E('div', { 'class': 'cbi-section' }, [
				E('div', { 'class': 'cbi-section-descr' }, 
					_('查看 AT WebServer 的实时系统日志')
				)
			])
		]);

		// 服务状态
		view.appendChild(
			E('div', { 'class': 'cbi-section' }, [
				E('div', { 'class': 'cbi-section-node' }, [
					E('div', { 'class': 'cbi-value' }, [
						E('label', { 'class': 'cbi-value-title' }, _('服务状态')),
						E('div', { 'class': 'cbi-value-field' }, [
							isRunning ? 
								E('span', { 'style': 'color: green' }, '● ' + _('运行中')) :
								E('span', { 'style': 'color: red' }, '● ' + _('已停止'))
						])
					]),
					E('div', { 'class': 'cbi-value' }, [
						E('label', { 'class': 'cbi-value-title' }, _('WebSocket 地址')),
						E('div', { 'class': 'cbi-value-field' }, [
							E('code', { 'id': 'ws-url' }, wsUrl)
						])
					]),
					E('div', { 'class': 'cbi-value' }, [
						E('label', { 'class': 'cbi-value-title' }, _('连接状态')),
						E('div', { 'class': 'cbi-value-field' }, [
							E('span', { 
								'id': 'connection-status',
								'style': 'color: gray'
							}, '● ' + _('未连接'))
						])
					])
				])
			])
		);

		if (!isRunning) {
			view.appendChild(
				E('div', { 'class': 'cbi-section' }, [
					E('div', { 'class': 'alert-message warning' }, [
						E('p', {}, _('服务未运行，请先启动 AT WebServer 服务')),
						E('p', {}, [
							E('a', { 
								'href': L.url('admin/services/at-webserver/config'),
								'class': 'btn cbi-button-apply'
							}, _('前往配置'))
						])
					])
				])
			);
		} else {
			// 控制按钮
			view.appendChild(
				E('div', { 'class': 'cbi-section' }, [
					E('div', { 'class': 'cbi-section-node' }, [
						E('div', { 'class': 'cbi-value' }, [
							E('div', { 'class': 'cbi-value-field' }, [
								E('button', {
									'id': 'pause-button',
									'class': 'btn cbi-button-action'
								}, _('暂停滚动')),
								' ',
								E('button', {
									'id': 'clear-button',
									'class': 'btn cbi-button-reset'
								}, _('清空屏幕')),
								' ',
								E('button', {
									'id': 'clear-file-button',
									'class': 'btn cbi-button-reset'
								}, _('清空日志文件'))
							])
						])
					])
				])
			);

			// 输出区域
			view.appendChild(
				E('div', { 'class': 'cbi-section' }, [
					E('div', { 'class': 'cbi-section-node' }, [
						E('pre', {
							'id': 'log-output',
							'style': 'background: #1e1e1e; color: #d4d4d4; padding: 15px; border-radius: 4px; height: 600px; overflow-y: auto; font-family: "Consolas", "Monaco", monospace; font-size: 13px; white-space: pre-wrap; word-wrap: break-word;'
						}, _('等待连接...'))
					])
				])
			);

			// WebSocket 逻辑
			view.appendChild(
				E('script', {}, `
(function() {
	var ws = null;
	var reconnectTimer = null;
	var isManualClose = false;
	var isAuthenticated = false;
	var authKey = '${wsAuthKeyEscaped}';
	var isPaused = false;
	
	// 性能优化配置
	var MAX_LINES = 1000;
	var BUFFER_FLUSH_INTERVAL = 200; // ms
	var logBuffer = [];
	var flushTimer = null;
	
	var statusEl = document.getElementById('connection-status');
	var outputEl = document.getElementById('log-output');
	var pauseBtn = document.getElementById('pause-button');
	var clearBtn = document.getElementById('clear-button');
	var clearFileBtn = document.getElementById('clear-file-button');
	
	function updateStatus(status, color) {
		statusEl.innerHTML = '● ' + status;
		statusEl.style.color = color;
	}
	
	// 批量更新 DOM
	function flushBuffer() {
		if (logBuffer.length === 0) return;
		
		var wasAtBottom = (outputEl.scrollHeight - outputEl.clientHeight <= outputEl.scrollTop + 50);
		
		// 构建 HTML 字符串
		var html = '';
		for (var i = 0; i < logBuffer.length; i++) {
			// 如果是系统日志对象，直接使用其数据
			if (typeof logBuffer[i] === 'object' && logBuffer[i].type === 'system_log') {
				html += '<span style="color: #ce9178;">' + logBuffer[i].data + '</span>\\n';
			} else {
				// 普通文本
				html += '<span style="color: #d4d4d4;">' + logBuffer[i] + '</span>\\n';
			}
		}
		
		// 临时创建一个 div 来追加内容，避免多次重排
		var tempDiv = document.createElement('div');
		tempDiv.innerHTML = html;
		
		// 将新内容追加到 outputEl
		while (tempDiv.firstChild) {
			outputEl.appendChild(tempDiv.firstChild);
		}
		
		// 限制行数
		// 注意：这种方式可能不太精确，因为 span 和 br 混在一起
		// 更高效的方式可能是直接操作 innerHTML 字符串，但要注意转义
		// 这里简单处理：如果子节点过多，移除前面的
		// 每个日志条目通常对应一个 span 和一个换行（在 pre 中换行是字符）
		// 由于我们使用的是 pre-wrap 和 \\n，实际上是一个 text node 或 span 列表
		// 简单起见，如果 scrollHeight 过大或子元素过多，清理一下
		
		// 优化：仅当缓冲区写入后检查
		if (outputEl.childNodes.length > MAX_LINES * 2) { // *2 是因为可能包含 text nodes
			// 移除旧节点直到数量合适
			while (outputEl.childNodes.length > MAX_LINES) {
				outputEl.removeChild(outputEl.firstChild);
			}
		}
		
		logBuffer = [];
		
		// 智能滚动
		if (!isPaused && wasAtBottom) {
			outputEl.scrollTop = outputEl.scrollHeight;
		}
	}
	
	function appendLog(data) {
		logBuffer.push(data);
		
		if (!flushTimer) {
			flushTimer = setTimeout(function() {
				flushBuffer();
				flushTimer = null;
			}, BUFFER_FLUSH_INTERVAL);
		}
	}
	
	function connect() {
		if (ws && (ws.readyState === WebSocket.CONNECTING || ws.readyState === WebSocket.OPEN)) {
			return;
		}
		
		isAuthenticated = false;
		updateStatus('连接中...', 'orange');
		appendLog('正在连接到 ${wsUrl}');
		
		ws = new WebSocket('${wsUrl}');
		
		ws.onopen = function() {
			if (authKey) {
				updateStatus('认证中...', 'orange');
				ws.send(JSON.stringify({ auth_key: authKey }));
			} else {
				isAuthenticated = true;
				updateStatus('已连接', 'green');
				appendLog('WebSocket 连接成功');
				if (reconnectTimer) {
					clearTimeout(reconnectTimer);
					reconnectTimer = null;
				}
				// 获取现有日志
				ws.send(JSON.stringify({ command: 'GET_SYS_LOGS' }));
			}
		};
		
		ws.onmessage = function(event) {
			try {
				var data = JSON.parse(event.data);
				
				// 处理认证响应
				if (!isAuthenticated && authKey) {
					if (data.success) {
						isAuthenticated = true;
						updateStatus('已连接', 'green');
						appendLog('身份验证成功');
						if (reconnectTimer) {
							clearTimeout(reconnectTimer);
							reconnectTimer = null;
						}
						// 获取现有日志
						ws.send(JSON.stringify({ command: 'GET_SYS_LOGS' }));
						return;
					} else if (data.error) {
						appendLog('认证失败: ' + (data.message || data.error));
						updateStatus('认证失败', 'red');
						ws.close();
						return;
					}
				}
				
				// 处理系统日志推送
				if (data.type === 'system_log') {
					appendLog(data);
				} else if (data.success !== undefined) {
					// 响应 GET_SYS_LOGS 或 CLEAR_SYS_LOGS
					if (data.data) {
						// 可能是历史日志，批量添加
						// 分割行并添加到缓冲区
						var lines = data.data.split('\\n');
						for (var i = 0; i < lines.length; i++) {
							if (lines[i].trim()) {
								logBuffer.push({ type: 'system_log', data: lines[i] });
							}
						}
						// 立即刷新一次
						if (!flushTimer) {
							flushBuffer();
						}
					}
				}
			} catch(e) {
				appendLog(event.data);
			}
		};
		
		ws.onerror = function(error) {
			appendLog('WebSocket 错误');
			updateStatus('连接错误', 'red');
		};
		
		ws.onclose = function() {
			updateStatus('未连接', 'gray');
			
			if (!isManualClose) {
				appendLog('连接断开，5秒后尝试重连...');
				reconnectTimer = setTimeout(connect, 5000);
			}
		};
	}
	
	// 暂停/继续按钮
	pauseBtn.onclick = function() {
		isPaused = !isPaused;
		if (isPaused) {
			pauseBtn.innerText = '继续滚动';
			pauseBtn.classList.remove('cbi-button-action');
			pauseBtn.classList.add('cbi-button-neutral');
		} else {
			pauseBtn.innerText = '暂停滚动';
			pauseBtn.classList.remove('cbi-button-neutral');
			pauseBtn.classList.add('cbi-button-action');
			// 恢复时滚动到底部
			outputEl.scrollTop = outputEl.scrollHeight;
		}
	};
	
	// 清空屏幕按钮
	clearBtn.onclick = function() {
		outputEl.innerHTML = '';
		logBuffer = [];
	};
	
	// 清空文件按钮
	clearFileBtn.onclick = function() {
		if (ws && ws.readyState === WebSocket.OPEN) {
			if (confirm('确定要清空服务器上的日志文件吗？此操作不可恢复。')) {
				ws.send(JSON.stringify({ command: 'CLEAR_SYS_LOGS' }));
				outputEl.innerHTML = '';
			}
		} else {
			alert('未连接到服务器');
		}
	};
	
	// 页面卸载时关闭连接
	window.addEventListener('beforeunload', function() {
		isManualClose = true;
		if (ws) {
			ws.close();
		}
	});
	
	// 自动连接
	connect();
})();
				`)
			);
		}

		return view;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
