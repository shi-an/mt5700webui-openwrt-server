mod config;
mod connection;
mod client;
mod server;
mod handlers;
mod notifications;
mod models;
mod pdu;
mod schedule;
mod network;

use config::Config;
use notifications::NotificationManager;
use client::ATClient;
use server::start_server;
use log::info;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();
    
    info!("Starting AT Webserver (Rust Version)...");
    
    let config = Config::load();
    let notifications = NotificationManager::new(config.notification_config.clone());
    
    let at_client = ATClient::new(config.clone(), notifications);
    let at_client_arc = Arc::new(at_client.clone());
    
    // Spawn schedule monitor
    let schedule_config = config.schedule_config.clone();
    let monitor_client = at_client_arc.clone();
    tokio::spawn(async move {
        schedule::monitor_loop(monitor_client, schedule_config).await;
    });

    // Start WebSocket server
    start_server(
        config.websocket_config.ipv4.port, 
        config.websocket_config.ipv6.port,
        config.websocket_config.auth_key.clone(),
        at_client
    ).await;
}
