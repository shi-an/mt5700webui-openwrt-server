'use strict';
'require view';

return view.extend({
	render: function() {
		var cssStr = 
			'body { overflow: hidden !important; } ' + 
			'footer, .footer, #footer, .luci-theme-argon-footer { display: none !important; } ' + 
			'#maincontent, #maincontent > div { padding: 0 !important; margin: 0 !important; max-width: 100% !important; height: 100% !important; display: flex !important; flex-direction: column !important; } ' + 
			'.cbi-map { margin-bottom: 0 !important; } ' + 
			'@media (min-width: 992px) { ' + 
				'.main-right { padding-left: 0 !important; width: 100% !important; } ' + 
				
				/* 恢复 Header 整体颜色和外层 Padding */ 
				'header, .header { position: relative !important; width: 100% !important; padding: 0 !important; margin: 0 !important; background-color: var(--primary, #5e72e4) !important; color: var(--header-color, #fff) !important; display: block !important; box-sizing: border-box !important; } ' + 
				'header > div { display: flex !important; padding: 0.8rem 0 !important; align-items: center !important; box-sizing: border-box !important; } ' + 

				/* 强行唤醒包含按钮和标题的 flex1 容器 */ 
				'body > div.main > div.main-right > header > div > div > div.flex1 { display: flex !important; flex: 1 !important; align-items: center !important; visibility: visible !important; opacity: 1 !important; } ' + 
				
				/* 【物理像素锁死】a.showSide 容器样式 */ 
				'body > div.main > div.main-right > header > div > div > div.flex1 > a.showSide { display: inline-flex !important; align-items: center !important; justify-content: center !important; margin: 0 !important; padding: 0 !important; position: relative !important; z-index: 99 !important; cursor: pointer !important; visibility: visible !important; opacity: 1 !important; vertical-align: middle !important; } ' + 
				
				/* 【原厂尺寸克隆】汉堡图标 SVG 本身：精确匹配 Argon 字体的 27.2px！ */ 
				'body > div.main > div.main-right > header > div > div > div.flex1 > a.showSide::before { content: "" !important; display: block !important; width: 27.2px !important; height: 27.2px !important; background-color: #fff !important; ' + 
				'-webkit-mask: url("data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCI+PHBhdGggZD0iTTMgNmgxOHYySDNWNm0wIDVoMTh2Mkgzdi0ybTAgNWgxOHYySDN2LTJ6Ii8+PC9zdmc+") no-repeat center / contain !important; ' + 
				'mask: url("data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAyNCAyNCI+PHBhdGggZD0iTTMgNmgxOHYySDNWNm0wIDVoMTh2Mkgzdi0ybTAgNWgxOHYySDN2LTJ6Ii8+PC9zdmc+") no-repeat center / contain !important; ' + 
				'visibility: visible !important; } ' + 
				'body > div.main > div.main-right > header > div > div > div.flex1 > a.showSide::after { display: none !important; } ' + 
				
				/* 【物理像素锁死】完美复刻 a.brand 的原厂属性！字体 24px，高度 32px，左内边距 16px */ 
				'body > div.main > div.main-right > header > div > div > div.flex1 > a.brand { display: inline-block !important; margin: 0 !important; padding: 0 0 0 16px !important; height: 32px !important; line-height: 32px !important; font-family: TypoGraphica, var(--font-family-sans-serif) !important; font-size: 24px !important; color: #fff !important; text-decoration: none !important; white-space: nowrap !important; vertical-align: middle !important; visibility: visible !important; opacity: 1 !important; box-sizing: border-box !important; } ' + 
				
				/* 侧边栏滑出逻辑 */ 
				'.main-left { position: fixed !important; left: -300px !important; z-index: 9999 !important; transition: left 0.3s ease !important; box-shadow: none !important; } ' + 
				'.main-left.active, .main-left.show, .main-left[style*="left: 0"], .main-left[style*="left:0"] { left: 0 !important; box-shadow: 4px 0 15px rgba(0,0,0,0.15) !important; transform: none !important; } ' + 
			'}'; 

		return E('div', { 
			class: 'cbi-map', 
			style: 'padding: 0; margin: 0; width: 100%; display: flex; flex-direction: column; overflow: hidden; height: calc(100vh - 60px);' 
		}, [ 
			E('style', {}, cssStr), 
			
			E('div', { 
				style: 'margin: 0 6px 4px 6px; flex-grow: 1; position: relative; border-radius: 8px; overflow: hidden; box-shadow: 0 4px 12px rgba(0,0,0,0.1);' 
			}, [ 
				E('iframe', { 
					src: '/5700/index.html', 
					style: 'width: 100%; height: 100%; border: none; display: block; background-color: var(--background-color, #fff);' 
				}) 
			]) 
		]); 
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});