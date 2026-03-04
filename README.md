# MT5700 WebUI OpenWrt Server

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/backend-Rust-orange.svg)
![LuCI](https://img.shields.io/badge/frontend-LuCI-green.svg)

专为 OpenWrt 设计的 5G 模组 (如 NRadio C8/MT5700) 管理套件，包含高性能 Rust 后端和现代化的 LuCI 前端界面。

## 🌟 核心组件

本项目包含两个主要部分：

1.  **`at-webserver` (Rust Backend)**
    *   高性能 WebSocket AT 命令服务器。
    *   **智能自动拨号**：自动检测模组上网/IMS通道，支持 IPv4/IPv6 双栈自动配置。
    *   **智能网卡探测**：自动识别 USB/WWAN/ETH 模组网卡。
    *   **定时锁频**：日间/夜间自动切换频段/频点锁定策略。
    *   **全能推送**：集成 PushPlus, Server酱, Telegram, 钉钉, 飞书, Bark 等 10+ 种通知通道。

2.  **`luci-app-at-webserver` (LuCI Frontend)**
    *   基于 OpenWrt LuCI 的 Web 管理界面。
    *   支持多选通知通道配置。
    *   可视化的定时锁频策略编辑器。
    *   内置 WebSocket 实时日志查看器和 AT 命令终端。

## 📥 下载与安装

### 预编译包
您可以从 Releases 页面下载最新的 `.ipk` 包进行安装：

```bash
# 上传 ipk 到路由器 /tmp 目录
opkg install /tmp/at-webserver_*.ipk
opkg install /tmp/luci-app-at-webserver_*.ipk
```

### 链接
*   **下载链接**: [点击这里下载](https://www.123865.com/s/BwcjVv-PexFd?pwd=GweY#) (提取码: GweY)

## 🖼️ 界面预览

| 概览 | 高级网络 |
|:---:|:---:|
| <img src="https://github.com/user-attachments/assets/229ee8de-6309-43c0-99a3-14cb36b770a2" width="400" /> | <img src="https://github.com/user-attachments/assets/cff3c45c-7d5c-4c77-af75-8e16fe94a25b" width="400" /> |

| 通知配置 | 实时日志 |
|:---:|:---:|
| <img src="https://github.com/user-attachments/assets/64d9ee66-6d4d-4005-b7de-93cdd3652162" width="400" /> | <img src="https://github.com/user-attachments/assets/a002f79c-335a-4dfd-9a5d-8df1a1dac736" width="400" /> |

## 🛠️ 编译指南

如果您想自己编译本项目：

1.  **准备 OpenWrt SDK**。
2.  **克隆仓库** 到 `package/` 目录：
    ```bash
    cd package/
    git clone https://github.com/shi-an/mt5700webui-openwrt-server.git
    ```
3.  **配置 Feeds** (确保包含 Rust 支持)：
    ```bash
    ./scripts/feeds update -a
    ./scripts/feeds install -a
    ```
4.  **选择包** (`make menuconfig`)：
    *   Network -> at-webserver
    *   LuCI -> Applications -> luci-app-at-webserver
5.  **编译**：
    ```bash
    make package/at-webserver/compile
    make package/luci-app-at-webserver/compile
    ```

## 🤝 贡献

欢迎提交 Issue 和 Pull Request！

## 📄 许可证

MIT License

## 🙏 致谢

本项目的部分设计灵感和实现参考了以下优秀项目：

*   [**QModem**](https://github.com/FUjr/QModem): 感谢 QModem 提供的关于 OpenWrt 模组管理的思路和部分实现参考。
