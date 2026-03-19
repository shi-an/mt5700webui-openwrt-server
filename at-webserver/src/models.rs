use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 全局 NDIS 断开事件广播器
/// ^NDISSTAT: 0 时由 NdisStatHandler 发送，dial_monitor 订阅后立即触发恢复
pub static NDIS_DISCONNECT_TX: OnceLock<broadcast::Sender<()>> = OnceLock::new();

pub fn get_ndis_disconnect_tx() -> &'static broadcast::Sender<()> {
    NDIS_DISCONNECT_TX.get_or_init(|| {
        let (tx, _) = broadcast::channel(8);
        tx
    })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ATResponse {
    pub success: bool,
    pub data: Option<String>,
    pub error: Option<String>,
}

impl ATResponse {
    pub fn ok(data: Option<String>) -> Self {
        Self {
            success: true,
            data,
            error: None,
        }
    }

    pub fn error(err: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(err),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SMS {
    pub index: String,
    pub sender: String,
    pub content: String,
    pub timestamp: String,
}

pub type CommandSender = mpsc::Sender<(String, oneshot::Sender<ATResponse>)>;

#[derive(Debug, Clone)]
pub enum ConnectionType {
    Network,
    Serial,
}
