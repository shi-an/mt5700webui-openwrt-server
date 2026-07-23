use async_trait::async_trait;
use anyhow::{Context, Result};
use log::{debug, info, warn};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Duration};
use tokio_serial::SerialPortBuilderExt;

const AUTO_SERIAL_PORT: &str = "auto";
const MT5700_USB_VENDOR_ID: &str = "3466";

#[async_trait]
pub trait ATConnection: Send {
    async fn connect(&mut self) -> Result<()>;
    async fn close(&mut self) -> Result<()>;
    async fn send(&mut self, data: &[u8]) -> Result<()>;
    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize>;
    fn is_connected(&self) -> bool;
}

pub struct NetworkATConnection {
    host: String,
    port: u16,
    timeout_secs: u64,
    stream: Option<TcpStream>,
}

impl NetworkATConnection {
    pub fn new(host: String, port: u16, timeout_secs: u64) -> Self {
        Self {
            host,
            port,
            timeout_secs,
            stream: None,
        }
    }
}

#[async_trait]
impl ATConnection for NetworkATConnection {
    async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        info!("Connecting to network AT server at {}", addr);
        match timeout(Duration::from_secs(self.timeout_secs), TcpStream::connect(&addr)).await {
            Ok(result) => {
                self.stream = Some(result.context("Failed to connect to network AT server")?);
                info!("Connected to network AT server");
                Ok(())
            }
            Err(_) => {
                anyhow::bail!("Connection timed out");
            }
        }
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            let _ = stream.shutdown().await;
        }
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            stream.write_all(data).await.context("Failed to write to stream")?;
            stream.flush().await.context("Failed to flush stream")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(stream) = &mut self.stream {
            // We just await data. Cancellation via timeout is handled by caller (client.rs: select!)
            stream.read(buffer).await.context("Failed to read from stream")
        } else {
            anyhow::bail!("Not connected");
        }
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

pub struct SerialATConnection {
    port: String,
    baudrate: u32,
    timeout_secs: u64,
    stream: Option<tokio_serial::SerialStream>,
}

impl SerialATConnection {
    pub fn new(port: String, baudrate: u32, timeout_secs: u64) -> Self {
        Self {
            port,
            baudrate,
            timeout_secs,
            stream: None,
        }
    }

    async fn connect_auto(&mut self) -> Result<()> {
        let candidates = enumerate_serial_candidates().await?;
        if candidates.is_empty() {
            anyhow::bail!("No ttyUSB or ttyACM devices were found");
        }

        info!(
            "Probing {} serial device(s) for the modem AT control channel...",
            candidates.len()
        );
        for candidate in &candidates {
            match open_and_probe_control_port(
                candidate,
                self.baudrate,
                self.timeout_secs.clamp(1, 3),
            ).await {
                Ok(port) => {
                    info!("Selected modem AT control channel: {}", candidate);
                    self.port = candidate.clone();
                    self.stream = Some(port);
                    return Ok(());
                }
                Err(e) => {
                    debug!("Serial device {} is not an AT control channel: {}", candidate, e);
                }
            }
        }

        anyhow::bail!(
            "No AT control channel responded on enumerated devices: {}",
            candidates.join(", ")
        )
    }
}

#[async_trait]
impl ATConnection for SerialATConnection {
    async fn connect(&mut self) -> Result<()> {
        if self.port.eq_ignore_ascii_case(AUTO_SERIAL_PORT) {
            return self.connect_auto().await;
        }

        info!("Opening serial port {} at {}", self.port, self.baudrate);
        let port = tokio_serial::new(&self.port, self.baudrate)
            .open_native_async()
            .context("Failed to open serial port")?;
        self.stream = Some(port);
        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.stream = None;
        Ok(())
    }

    async fn send(&mut self, data: &[u8]) -> Result<()> {
        if let Some(stream) = &mut self.stream {
            stream.write_all(data).await.context("Failed to write to serial")?;
            stream.flush().await.context("Failed to flush serial")?;
            Ok(())
        } else {
            anyhow::bail!("Not connected");
        }
    }

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if let Some(stream) = &mut self.stream {
             // Serial reading doesn't inherently timeout in the same way, but we can wrap it.
             // Usually we just read.
             stream.read(buffer).await.context("Failed to read from serial")
        } else {
            anyhow::bail!("Not connected");
        }
    }

     fn is_connected(&self) -> bool {
        self.stream.is_some()
    }
}

async fn enumerate_serial_candidates() -> Result<Vec<String>> {
    let mut entries = fs::read_dir("/dev")
        .await
        .context("Failed to enumerate /dev for serial control channels")?;
    let mut candidates = Vec::new();

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_serial_candidate_name(&name) {
            continue;
        }

        let preferred = serial_device_has_vendor(&name, MT5700_USB_VENDOR_ID).await;
        candidates.push((preferred, format!("/dev/{}", name)));
    }

    // When the MT5700 is visible in sysfs, do not send probe commands to
    // unrelated serial hardware. Otherwise probe every conventional USB port.
    let has_preferred = candidates.iter().any(|(preferred, _)| *preferred);
    if has_preferred {
        candidates.retain(|(preferred, _)| *preferred);
    }
    candidates.sort_by(|left, right| left.1.cmp(&right.1));

    Ok(candidates.into_iter().map(|(_, path)| path).collect())
}

fn is_serial_candidate_name(name: &str) -> bool {
    ["ttyUSB", "ttyACM"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .map(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false)
    })
}

async fn serial_device_has_vendor(device_name: &str, vendor_id: &str) -> bool {
    let sysfs_path = format!("/sys/class/tty/{}/device", device_name);
    let Ok(mut current) = fs::canonicalize(sysfs_path).await else {
        return false;
    };

    for _ in 0..10 {
        if let Ok(value) = fs::read_to_string(current.join("idVendor")).await {
            return value.trim().eq_ignore_ascii_case(vendor_id);
        }
        if !current.pop() {
            break;
        }
    }
    false
}

async fn open_and_probe_control_port(
    path: &str,
    baudrate: u32,
    timeout_secs: u64,
) -> Result<tokio_serial::SerialStream> {
    let mut port = tokio_serial::new(path, baudrate)
        .open_native_async()
        .with_context(|| format!("Failed to open {}", path))?;

    sleep(Duration::from_millis(100)).await;
    port.write_all(b"AT\r")
        .await
        .with_context(|| format!("Failed to write AT probe to {}", path))?;
    port.flush().await?;

    let probe = async {
        let mut response = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let count = port.read(&mut buffer).await?;
            if count == 0 {
                anyhow::bail!("Serial device closed during AT probe");
            }
            response.extend_from_slice(&buffer[..count]);

            if response_has_terminal_line(&response, "OK") {
                return Ok::<(), anyhow::Error>(());
            }
            if response_has_terminal_line(&response, "ERROR") {
                anyhow::bail!("AT probe returned ERROR");
            }
            if response.len() > 8192 {
                warn!("Discarding oversized probe response from {}", path);
                anyhow::bail!("AT probe response exceeded 8192 bytes");
            }
        }
    };

    timeout(Duration::from_secs(timeout_secs), probe)
        .await
        .with_context(|| format!("Timed out probing {}", path))??;
    Ok(port)
}

fn response_has_terminal_line(response: &[u8], expected: &str) -> bool {
    String::from_utf8_lossy(response)
        .split(['\r', '\n'])
        .any(|line| line.trim() == expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_serial_device_names() {
        assert!(is_serial_candidate_name("ttyUSB0"));
        assert!(is_serial_candidate_name("ttyACM12"));
        assert!(!is_serial_candidate_name("ttyS0"));
        assert!(!is_serial_candidate_name("ttyUSB"));
    }

    #[test]
    fn at_probe_requires_a_complete_terminal_line() {
        assert!(response_has_terminal_line(b"AT\r\r\nOK\r\n", "OK"));
        assert!(response_has_terminal_line(b"AT\rOK\r", "OK"));
        assert!(!response_has_terminal_line(b"TOKEN\r\n", "OK"));
    }
}
