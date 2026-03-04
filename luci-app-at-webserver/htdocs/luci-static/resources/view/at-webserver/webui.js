'use strict';
'require view';

return view.extend({
	render: function() {
		return E('div', {
			class: 'cbi-map',
			// 1. 强制铺满父级 Flex 容器，并移除所有干扰边距
			style: 'padding: 0; margin: 0; width: 100%; display: flex; flex-direction: column; flex-grow: 1; height: calc(100vh - 58px);'
		}, [
			// 2. 注入针对 Argon 主题的“沉浸式”补丁
			E('style', {}, `
				/* 锁死外层滚动条 */
				body { overflow: hidden !important; }
				/* 消除 LuCI 默认页脚 */
				footer, .footer, #footer, .luci-theme-argon-footer { display: none !important; }
				/* 穿透修改 LuCI 核心容器样式，确保左右 10px 边距生效 */
				#maincontent > div {
					padding: 0 !important;
					margin: 0 !important;
					max-width: 100% !important;
					display: flex !important;
					flex-direction: column !important;
					height: 100% !important;
				}
			`),
			
			// 3. 模组面板容器：使用你要求的 margin 逻辑，并通过 flex 控制高度
			E('div', {
				style: 'margin: 0 0px 0px 0px; flex-grow: 1; position: relative; border-radius: 8px; overflow: hidden; box-shadow: 0 4px 12px rgba(0,0,0,0.1);'
			}, [
				E('iframe', {
					src: '/5700/index.html',
					// 使用 100% 继承容器高度，不再使用 103vh
					style: 'width: 100%; height: 100%; border: none; display: block; background-color: #fff;'
				})
			])
		]);
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});