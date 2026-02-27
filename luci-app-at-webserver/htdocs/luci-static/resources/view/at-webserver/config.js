'use strict';
'require view';
'require form';
'require uci';
'require rpc';
'require fs';
'require ui';

var callServiceList = rpc.declare({
	object: 'service',
	method: 'list',
	params: ['name'],
	expect: { '': {} }
});

var callInitAction = rpc.declare({
	object: 'luci',
	method: 'setInitAction',
	params: ['name', 'action'],
	expect: { result: false }
});

function getServiceStatus() {
	return L.resolveDefault(callServiceList('at-webserver'), {}).then(function(res) {
		var isRunning = false;
		try {
			isRunning = res['at-webserver']['instances']['instance1']['running'];
		} catch(e) { }
		return isRunning;
	});
}

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('at-webserver'),
			getServiceStatus()
		]);
	},

	render: function(data) {
		var isRunning = data[1];
		var m, s, o;

		m = new form.Map('at-webserver', _('AT WebServer'),
			_('WebSocket服务器，用于通过Web界面管理AT命令。'));

		s = m.section(form.NamedSection, 'config', 'at-webserver');
		s.addremove = false;
		s.anonymous = false;

		// 分页定义
		s.tab('general', _('基本设置'));
		s.tab('network', _('高级网络'));
		s.tab('websocket', _('WebSocket'));
		s.tab('notify', _('通知与日志'));

		// --- 基本设置 ---
		
		// 服务状态显示
		o = s.taboption('general', form.DummyValue, '_status', _('服务状态'));
		o.cfgvalue = function() {
			return isRunning ? 
				'<span style="color:green">● ' + _('运行中') + '</span>' : 
				'<span style="color:red">● ' + _('已停止') + '</span>';
		};
		o.rawhtml = true;

		// 启用开关
		o = s.taboption('general', form.Flag, 'enabled', _('启用服务'),
			_('启用后服务将在系统启动时自动运行'));
		o.rmempty = false;

		// 连接类型
		o = s.taboption('general', form.ListValue, 'connection_type', _('连接类型'),
			_('选择AT命令的连接方式'));
		o.value('NETWORK', _('网络连接'));
		o.value('SERIAL', _('串口连接'));
		o.default = 'NETWORK';
		o.rmempty = false;

		// 网络连接配置
		o = s.taboption('general', form.Value, 'network_host', _('网络主机'),
			_('AT命令服务的IP地址'));
		o.datatype = 'host';
		o.default = '192.168.8.1';
		o.depends('connection_type', 'NETWORK');

		o = s.taboption('general', form.Value, 'network_port', _('网络端口'),
			_('AT命令服务的端口号'));
		o.datatype = 'port';
		o.default = '20249';
		o.depends('connection_type', 'NETWORK');

		o = s.taboption('general', form.Value, 'network_timeout', _('网络超时'),
			_('网络连接超时时间（秒）'));
		o.datatype = 'uinteger';
		o.default = '10';
		o.depends('connection_type', 'NETWORK');

		// 模块访问安全配置
		o = s.taboption('general', form.DummyValue, '_module_security_title', _('模块访问安全'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return '<strong style="color:#0099CC;">━━━━━━━ 模块 (192.168.8.1:20249) 访问控制 ━━━━━━━</strong>';
		};
		o.depends('connection_type', 'NETWORK');

		o = s.taboption('general', form.Flag, 'network_allow_wan', _('☐ 允许外网访问模块'),
			_('是否允许从外网直接访问模块的 AT 端口。<br><strong style="color:red;">⚠️ 安全警告：</strong>开启此选项将允许任何人从外网访问模块，存在严重安全风险！<br><strong>建议：</strong>保持关闭，仅通过 WebSocket 管理。<br><em>注意：此选项仅在"网络连接"模式下生效。</em>'));
		o.default = '0';
		o.rmempty = false;
		o.depends('connection_type', 'NETWORK');

		o = s.taboption('general', form.Flag, 'network_restrict_access', _('☐ 限制模块局域网访问'),
			_('启用后，只有路由器本身可以访问模块（192.168.8.1:20249），局域网其他设备将无法访问。<br><strong>适用场景：</strong>防止局域网设备直接访问模块，统一通过 WebSocket 管理。<br><em>注意：此选项仅在"网络连接"模式下生效。</em>'));
		o.default = '0';
		o.rmempty = false;
		o.depends('connection_type', 'NETWORK');

		// 串口连接配置
		o = s.taboption('general', form.ListValue, 'serial_port', _('串口设备'),
			_('选择串口设备或手动输入路径'));
		o.depends('connection_type', 'SERIAL');
		
		// 动态添加系统中可用的串口设备
		o.load = function(section_id) {
			return Promise.all([
				fs.list('/dev').catch(function() { return []; }),
				form.ListValue.prototype.load.apply(this, [section_id])
			]).then(L.bind(function(results) {
				var devices = results[0] || [];
				var currentValue = results[1];
				
				this.keylist = [];
				this.vallist = [];
				
				var serialDevices = [];
				devices.forEach(function(item) {
					var name = item.name;
					if (name.match(/^tty(USB|ACM|S)\d+$/)) {
						serialDevices.push('/dev/' + name);
					}
				});
				
				serialDevices.sort();
				
				if (serialDevices.length > 0) {
					serialDevices.forEach(L.bind(function(dev) {
						this.value(dev, dev);
					}, this));
				} else {
					this.value('/dev/ttyUSB0', '/dev/ttyUSB0 (默认)');
				}
				
				this.value('custom', _('自定义路径...'));
				
				if (currentValue && !serialDevices.includes(currentValue) && currentValue !== 'custom') {
					this.value(currentValue, currentValue + ' (当前)');
				}
				
				return currentValue;
			}, this));
		};
		o.default = '/dev/ttyUSB0';
		
		o = s.taboption('general', form.Value, 'serial_port_custom', _('自定义串口路径'),
			_('输入完整的串口设备路径'));
		o.depends('serial_port', 'custom');
		o.placeholder = '/dev/ttyUSB0';
		o.rmempty = false;

		o = s.taboption('general', form.ListValue, 'serial_baudrate', _('波特率'),
			_('串口通信波特率'));
		o.value('9600', '9600');
		o.value('19200', '19200');
		o.value('38400', '38400');
		o.value('57600', '57600');
		o.value('115200', '115200');
		o.value('230400', '230400');
		o.value('460800', '460800');
		o.value('921600', '921600');
		o.default = '115200';
		o.depends('connection_type', 'SERIAL');

		o = s.taboption('general', form.Value, 'serial_timeout', _('串口超时'),
			_('串口通信超时时间（秒）'));
		o.datatype = 'uinteger';
		o.default = '10';
		o.depends('connection_type', 'SERIAL');

		// --- 高级网络 ---
		
		o = s.taboption('network', form.ListValue, 'pdp_type', _('PDP 类型'), _('选择拨号协议类型'));
		o.value('ipv4', 'IPv4 Only');
		o.value('ipv6', 'IPv6 Only');
		o.value('ipv4v6', 'IPv4 + IPv6');
		o.default = 'ipv4v6';
		
		o = s.taboption('network', form.Flag, 'ra_master', _('IPv6 RA Master'), _('启用后作为 IPv6 RA 主设备（分配 IPv6 地址）'));
		o.default = '0';
		o.depends('pdp_type', 'ipv6');
		o.depends('pdp_type', 'ipv4v6');
		
		o = s.taboption('network', form.Flag, 'extend_prefix', _('IPv6 扩展前缀'), _('启用 IPv6 扩展前缀功能'));
		o.default = '1';
		o.depends('pdp_type', 'ipv6');
		o.depends('pdp_type', 'ipv4v6');
		
		o = s.taboption('network', form.Flag, 'do_not_add_dns', _('禁用自动 DNS'), _('不使用运营商下发的 DNS 服务器'));
		o.default = '0';
		
		o = s.taboption('network', form.DynamicList, 'dns_list', _('自定义 DNS'), _('指定使用的 DNS 服务器地址'));
		o.datatype = 'ipaddr';
		o.depends('do_not_add_dns', '0');

		// 1. 添加标题和主开关
		o = s.taboption('network', form.DummyValue, '_schedule_title', '<br/><strong style="color:#0099CC;">━━━━━━━ 定时锁频 (计划任务) ━━━━━━━</strong>');
		o.rawhtml = true;
		o = s.taboption('network', form.Flag, 'schedule_enabled', _('启用定时锁频'));
		o.default = '0';

		// 2. 添加基础设置 (需 depends('schedule_enabled', '1'))
		o = s.taboption('network', form.Value, 'schedule_check_interval', _('检测间隔 (秒)'));
		o.datatype = 'uinteger';
		o.default = '60';
		o.depends('schedule_enabled', '1');

		o = s.taboption('network', form.Value, 'schedule_timeout', _('无服务超时 (秒)'));
		o.datatype = 'uinteger';
		o.default = '180';
		o.depends('schedule_enabled', '1');

		o = s.taboption('network', form.Flag, 'schedule_unlock_lte', _('恢复时解锁 LTE'));
		o.default = '1';
		o.depends('schedule_enabled', '1');

		o = s.taboption('network', form.Flag, 'schedule_unlock_nr', _('恢复时解锁 NR'));
		o.default = '1';
		o.depends('schedule_enabled', '1');

		o = s.taboption('network', form.Flag, 'schedule_toggle_airplane', _('切换飞行模式生效'));
		o.default = '1';
		o.depends('schedule_enabled', '1');

		// 3. 核心精简：使用数组遍历生成 日间/夜间 模式表单，防止代码冗长
		const modes = [
			{ prefix: 'night', name: '夜间', start: '22:00', end: '06:00' },
			{ prefix: 'day', name: '日间' }
		];

		modes.forEach(mode => {
			o = s.taboption('network', form.SectionValue, `_${mode.prefix}_mode_title`, form.NamedSection, 'config', 'at-webserver', `>>> ${mode.name}模式设置 <<<`);
			o.depends('schedule_enabled', '1');

			o = s.taboption('network', form.Flag, `schedule_${mode.prefix}_enabled`, _(`启用${mode.name}模式`));
			o.default = '1';
			o.depends('schedule_enabled', '1');

			if (mode.prefix === 'night') {
				o = s.taboption('network', form.Value, `schedule_${mode.prefix}_start`, _('开始时间'));
				o.placeholder = mode.start;
				o.default = mode.start;
				o.depends(`schedule_${mode.prefix}_enabled`, '1');

				o = s.taboption('network', form.Value, `schedule_${mode.prefix}_end`, _('结束时间'));
				o.placeholder = mode.end;
				o.default = mode.end;
				o.depends(`schedule_${mode.prefix}_enabled`, '1');
			}

			['lte', 'nr'].forEach(net => {
				const netName = net.toUpperCase();
				const optPrefix = `schedule_${mode.prefix}_${net}`;
				
				// 添加 type
				o = s.taboption('network', form.ListValue, `${optPrefix}_type`, _(`${mode.name} ${netName} 锁定类型`));
				o.value('0', _('解锁'));
				o.value('1', _('频点锁定'));
				o.value('2', _('小区锁定'));
				o.value('3', _('频段锁定'));
				o.default = '3';
				o.depends(`schedule_${mode.prefix}_enabled`, '1');

				// 添加 bands
				o = s.taboption('network', form.Value, `${optPrefix}_bands`, _(`${mode.name} ${netName} 频段`));
				o.placeholder = net === 'lte' ? '3,8' : '78,41';
				o.depends(`${optPrefix}_type`, '1');
				o.depends(`${optPrefix}_type`, '2');
				o.depends(`${optPrefix}_type`, '3');
				o.depends('schedule_enabled', '1'); // 额外依赖

				// 添加 arfcns
				o = s.taboption('network', form.Value, `${optPrefix}_arfcns`, _(`${mode.name} ${netName} 频点`));
				o.depends(`${optPrefix}_type`, '1');
				o.depends(`${optPrefix}_type`, '2');
				o.depends('schedule_enabled', '1'); // 额外依赖

				// 如果是 nr，添加 scs_types
				if (net === 'nr') {
					o = s.taboption('network', form.Value, `${optPrefix}_scs_types`, _(`${mode.name} NR SCS 类型`));
					o.placeholder = '1,1';
					o.depends(`${optPrefix}_type`, '2');
					o.depends('schedule_enabled', '1'); // 额外依赖
				}

				// 添加 pcis
				o = s.taboption('network', form.Value, `${optPrefix}_pcis`, _(`${mode.name} ${netName} PCI`));
				o.depends(`${optPrefix}_type`, '2');
				o.depends('schedule_enabled', '1'); // 额外依赖
			});
		});

		// --- WebSocket ---

		o = s.taboption('websocket', form.Value, 'websocket_port', _('WebSocket 端口'),
			_('WebSocket服务器监听端口'));
		o.datatype = 'port';
		o.default = '8765';

		o = s.taboption('websocket', form.Flag, 'websocket_allow_wan', _('☐ 允许外网访问 WebSocket'),
			_('是否允许从外网访问 WebSocket。启用后将自动配置防火墙规则。<br><strong>安全提示：</strong>如果允许外网访问，强烈建议设置连接密钥！'));
		o.rmempty = false;
		o.default = '0';

		o = s.taboption('websocket', form.Value, 'websocket_auth_key', _('连接密钥'),
			_('WebSocket 连接密钥，用于验证客户端身份。<br>留空则不进行验证（不安全！）<br>建议使用复杂的随机字符串。'));
		o.password = true;
		o.placeholder = '留空表示不验证';
		o.rmempty = true;

		// --- 通知与日志 ---

		// 系统日志配置
		o = s.taboption('notify', form.DummyValue, '_syslog_title', _('系统日志'));
		o.rawhtml = true;
		o.cfgvalue = function() { return '<h3>' + _('系统运行日志配置') + '</h3>'; };

		o = s.taboption('notify', form.Flag, 'sys_log_enable', _('启用系统日志'), _('记录系统运行日志'));
		o.default = '1';

		o = s.taboption('notify', form.Flag, 'sys_log_persist', _('持久化日志'), _('将日志保存到非易失存储（重启后保留）'));
		o.default = '0';
		o.depends('sys_log_enable', '1');

		o = s.taboption('notify', form.Value, 'sys_log_path_temp', _('临时日志路径'), _('临时日志文件路径（内存中）'));
		o.default = '/tmp/at-webserver.log';
		o.depends('sys_log_persist', '0');

		o = s.taboption('notify', form.Value, 'sys_log_path_persist', _('持久日志路径'), _('持久化日志文件路径'));
		o.default = '/etc/at-webserver.log';
		o.depends('sys_log_persist', '1');

		// 通知配置
		o = s.taboption('notify', form.DummyValue, '_notify_title', _('事件通知'));
		o.rawhtml = true;
		o.cfgvalue = function() { return '<h3>' + _('短信与事件通知配置') + '</h3>'; };

		o = s.taboption('notify', form.Value, 'wechat_webhook', _('企业微信 Webhook'),
			_('企业微信机器人的 Webhook 地址，留空则不启用微信通知'));
		o.placeholder = 'https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=...';

		o = s.taboption('notify', form.Value, 'log_file', _('通知记录文件'),
			_('保存通知记录的日志文件路径，留空则不启用日志记录'));
		o.placeholder = '/var/log/at-notifications.log';

		o = s.taboption('notify', form.Flag, 'notify_sms', _('短信通知'), _('接收到新短信时发送通知'));
		o.default = '1';

		o = s.taboption('notify', form.Flag, 'notify_call', _('来电通知'), _('来电时发送通知'));
		o.default = '1';

		o = s.taboption('notify', form.Flag, 'notify_memory_full', _('存储满通知'), _('短信存储空间满时发送警告'));
		o.default = '1';

		o = s.taboption('notify', form.Flag, 'notify_signal', _('信号变化通知'), _('网络信号强度变化或制式切换时发送通知'));
		o.default = '1';

		return m.render();
	},

	handleSaveApply: function(ev, mode) {
		return this.handleSave(ev).then(L.bind(function() {
			// 等待一下确保 UCI 已提交
			return new Promise(function(resolve) {
				setTimeout(resolve, 500);
			}).then(L.bind(function() {
				return this.handleRestart(ev);
			}, this));
		}, this));
	},

	handleSave: function(ev) {
		// 直接调用父类 handleSave，让 LuCI 处理所有数据绑定和保存
		return this.super('handleSave', [ev]).then(function() {
			ui.addNotification(null, E('p', _('✓ 配置已保存')), 'success');
		}).catch(function(e) {
			ui.addNotification(null, E('p', _('保存配置失败: ') + (e.message || e)), 'error');
			throw e;
		});
	},

	handleRestart: function(ev) {
		ui.showModal(_('正在应用配置'), [
			E('p', { 'class': 'spinning' }, _('正在应用配置并重启服务...'))
		]);

		// 重新加载 UCI 以获取最新的 enabled 状态
		return uci.load('at-webserver').then(function() {
			var enabled = uci.get('at-webserver', 'config', 'enabled');
			
			if (enabled === '1') {
				// 启用并重启
				return callInitAction('at-webserver', 'enable').then(function() {
					return callInitAction('at-webserver', 'restart');
				});
			} else {
				// 停止并禁用
				return callInitAction('at-webserver', 'stop').then(function() {
					return callInitAction('at-webserver', 'disable');
				});
			}
		}).then(function() {
			return new Promise(function(resolve) { 
				setTimeout(resolve, 3000); 
			});
		}).then(function() {
			ui.hideModal();
			ui.addNotification(null, E('p', _('✓ 服务配置已应用')), 'success');
			setTimeout(function() { 
				window.location.reload(true); 
			}, 1000);
		}).catch(function(e) {
			ui.hideModal();
			ui.addNotification(null, E('p', _('应用配置失败: ') + (e.message || e)), 'error');
		});
	},

	handleReset: null
});
