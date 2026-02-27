use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::OnceLock;
use tokio::sync::broadcast;
use tokio::io::AsyncWriteExt;
use chrono::Local;
use crate::config::Config;

static LOGGER: OnceLock<AppLogger> = OnceLock::new();
static LOG_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();

pub struct AppLogger;

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S");
            let log_msg = format!("{} [{}] {}: {}", now, record.level(), record.target(), record.args());

            // Print to console (captured by procd/logread)
            println!("{}", log_msg);

            // Send to broadcast channel
            if let Some(tx) = LOG_CHANNEL.get() {
                // Ignore error if no receivers
                let _ = tx.send(log_msg);
            }
        }
    }

    fn flush(&self) {}
}

pub fn init(config: &Config) -> broadcast::Receiver<String> {
    // Initialize broadcast channel
    let (tx, rx) = broadcast::channel(100);
    LOG_CHANNEL.set(tx).expect("Failed to set log channel");

    // Initialize logger
    let logger = LOGGER.get_or_init(|| AppLogger);
    log::set_logger(logger).map(|()| log::set_max_level(LevelFilter::Info)).expect("Failed to set logger");

    // Start background task if logging is enabled
    if config.sys_log_config.enable {
        let log_path = if config.sys_log_config.persist {
            config.sys_log_config.path_persist.clone()
        } else {
            config.sys_log_config.path_temp.clone()
        };

        let mut rx_file = LOG_CHANNEL.get().unwrap().subscribe();
        
        tokio::spawn(async move {
            use tokio::fs::OpenOptions;
            
            // Ensure directory exists if needed? Usually /tmp or /etc exists.
            
            loop {
                if let Ok(msg) = rx_file.recv().await {
                    // Open file in append mode each time or keep open? 
                    // Keeping open is better for performance, but reopening handles log rotation better if external tools rotate it.
                    // For simplicity and robustness, we can try to keep it open or reopen on error.
                    // Let's try to append simply.
                    
                    let result = async {
                        let mut file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&log_path)
                            .await?;
                        file.write_all(msg.as_bytes()).await?;
                        file.write_all(b"\n").await?;
                        Ok::<(), std::io::Error>(())
                    }.await;

                    if let Err(e) = result {
                        eprintln!("Failed to write to log file: {}", e);
                    }
                }
            }
        });
    }

    rx
}
