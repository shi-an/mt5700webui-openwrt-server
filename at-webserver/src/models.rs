use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModemSessionState {
    Connected,
    Connecting,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModemSessionEvent {
    pub cid: Option<u8>,
    pub state: ModemSessionState,
    pub error_code: Option<u32>,
    pub pdp_type: Option<String>,
}

/// ^NDISSTAT is the modem-side data-session event. It is independent from the
/// OpenWrt interface state and is therefore broadcast as structured data.
pub static MODEM_SESSION_TX: OnceLock<broadcast::Sender<ModemSessionEvent>> = OnceLock::new();

pub fn get_modem_session_tx() -> &'static broadcast::Sender<ModemSessionEvent> {
    MODEM_SESSION_TX.get_or_init(|| {
        let (tx, _) = broadcast::channel(16);
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
