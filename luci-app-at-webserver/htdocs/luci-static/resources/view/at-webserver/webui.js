'use strict';
'require view';
'require dom';

return view.extend({
    render: function() {
        // 创建一个全屏 iframe 容器，嵌入 Vue 前端页面
        // 使用相对路径 '/5700/' 自动匹配当前路由器 IP
        return E('iframe', {
            src: '/5700/',
            style: 'width: 100%; height: 85vh; border: none; border-radius: 4px; box-shadow: 0 0 5px rgba(0,0,0,0.05);'
        });
    },

    // 隐藏 LuCI 底部的“保存并应用”、“保存”、“重置”按钮
    // 因为这个页面是只读展示或由内部 Vue 应用自行管理状态
    handleSaveApply: null,
    handleSave: null,
    handleReset: null
});
