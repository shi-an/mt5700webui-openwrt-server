use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use tokio::sync::broadcast;
use chrono::Local;
use crate::config::Config;
use std::sync::mpsc;
use std::thread;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use std::fs::{self, OpenOptions};
use std::io::Write;

static LOGGER: OnceLock<AppLogger> = OnceLock::new();
static LOG_LEVEL: AtomicU8 = AtomicU8::new(3);
static LOG_CHANNEL: OnceLock<broadcast::Sender<String>> = OnceLock::new();
static FILE_CHANNEL: OnceLock<mpsc::Sender<String>> = OnceLock::new();

pub struct AppLogger;

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        let level = match LOG_LEVEL.load(Ordering::Relaxed) {
            1 => LevelFilter::Error,
            2 => LevelFilter::Warn,
            4 => LevelFilter::Debug,
            _ => LevelFilter::Info,
        };
        metadata.level() <= level.to_level().unwrap_or(Level::Info)
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S");
            let log_msg = format!("{} [{}] {}: {}", now, record.level(), record.target(), record.args());

            // Print to console (captured by procd/logread)
            println!("{}", log_msg);

            // Send to broadcast channel (for WebSocket)
            if let Some(tx) = LOG_CHANNEL.get() {
                let _ = tx.send(log_msg.clone());
            }

            // Send to file writer thread
            if let Some(tx) = FILE_CHANNEL.get() {
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

    let level = match config.sys_log_config.level.as_str() {
        "error" => 1,
        "warn" => 2,
        "debug" => 4,
        _ => 3,
    };
    LOG_LEVEL.store(level, Ordering::Relaxed);

    // Initialize logger
    let logger = LOGGER.get_or_init(|| AppLogger);
    log::set_logger(logger).map(|()| log::set_max_level(LevelFilter::Debug)).expect("Failed to set logger");

    // Start background thread if logging is enabled
    if config.sys_log_config.enable {
        let log_path = if config.sys_log_config.persist {
            PathBuf::from("/var/log/at-webserver.log")
        } else {
            PathBuf::from("/tmp/at-webserver.log")
        };

        // Create directory if needed
        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        
        // Create empty file if not exists
        let _ = OpenOptions::new().create(true).write(true).open(&log_path);

        let (tx, rx) = mpsc::channel::<String>();
        FILE_CHANNEL.set(tx).expect("Failed to set file channel");

        thread::spawn(move || {
            let mut buffer = String::with_capacity(10240);
            let mut last_flush = Instant::now();

            loop {
                // Try to receive a message with a short timeout to allow periodic flushing
                if let Ok(msg) = rx.recv_timeout(Duration::from_millis(100)) {
                    buffer.push_str(&msg);
                    buffer.push('\n');
                }

                let should_flush = buffer.len() > 8192 || last_flush.elapsed() >= Duration::from_secs(5);
                
                if should_flush && !buffer.is_empty() {
                    // Check rotation
                    if let Ok(metadata) = fs::metadata(&log_path) {
                        if metadata.len() > 1024 * 1024 {
                            let mut bak_path = log_path.clone();
                            bak_path.set_extension("log.bak");
                            let _ = fs::rename(&log_path, bak_path);
                        }
                    }

                    // Write to file
                    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
                        let _ = file.write_all(buffer.as_bytes());
                    }
                    
                    buffer.clear();
                    last_flush = Instant::now();
                }
            }
        });
    }

    rx
}
