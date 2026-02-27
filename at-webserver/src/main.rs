mod config;
mod connection;
mod client;
mod server;
mod handlers;
mod notifications;
mod models;

use config::Config;
use notifications::NotificationManager;
use client::ATClient;
use server::start_server;
use log::info;

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
    
    start_server(config.websocket_config.ipv4.port, at_client).await;
}
