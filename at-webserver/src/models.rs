use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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
