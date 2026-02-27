use crate::client::ATClient;
use crate::config::ScheduleConfig;
use crate::models::ATResponse;
use crate::notifications::NotificationType;
use anyhow::{anyhow, Result};
use chrono::{Local, NaiveTime};
use log::{error, info, warn, debug};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::time::{sleep, Duration, Instant};

pub async fn monitor_loop(client: Arc<ATClient>, config: ScheduleConfig) {
    if !config.enabled {
        info!("Schedule frequency lock is disabled.");
        return;
    }

    info!("Starting schedule frequency lock monitor...");
    info!("  Check interval: {}s", config.check_interval);
    info!("  Timeout: {}s", config.timeout);
    info!("  Night mode: {} ({}-{})", if config.night_enabled { "Enabled" } else { "Disabled" }, config.night_start, config.night_end);
    info!("  Day mode: {}", if config.day_enabled { "Enabled" } else { "Disabled" });

    let mut last_service_time = Instant::now();
    let mut current_mode: Option<String> = None;
    let mut switch_count = 0;

    loop {
        // Determine current mode
        let target_mode = get_current_mode(&config);

        if target_mode != current_mode {
            if let Some(mode) = &target_mode {
                info!("Mode switch detected: {:?} -> {}", current_mode, mode);
                switch_count += 1;
                if let Err(e) = set_frequency_lock(&client, &config, mode, switch_count).await {
                    error!("Failed to set frequency lock for mode {}: {}", mode, e);
                } else {
                    current_mode = Some(mode.clone());
                }
            } else if current_mode.is_some() {
                // Target is None (no lock needed), but we are in a mode. Unlock everything.
                info!("No lock required for current time. Unlocking all.");
                if let Err(e) = unlock_all(&client, &config).await {
                    error!("Failed to unlock all: {}", e);
                } else {
                    current_mode = None;
                }
            }
        }

        // Check network status
        match check_network_status(&client).await {
            Ok(has_service) => {
                if has_service {
                    last_service_time = Instant::now();
                } else {
                    let no_service_duration = last_service_time.elapsed().as_secs();
                    if no_service_duration >= config.timeout {
                        warn!("Network service lost for {}s. Executing recovery (unlock all).", no_service_duration);
                        if let Err(e) = unlock_all(&client, &config).await {
                            error!("Recovery failed: {}", e);
                        }
                        last_service_time = Instant::now(); // Reset timer to avoid spamming recovery
                    } else {
                        debug!("No service for {}s", no_service_duration);
                    }
                }
            }
            Err(e) => {
                error!("Failed to check network status: {}", e);
            }
        }

        sleep(Duration::from_secs(config.check_interval)).await;
    }
}

fn get_current_mode(config: &ScheduleConfig) -> Option<String> {
    let now = Local::now().time();
    
    // Parse times
    let night_start = NaiveTime::parse_from_str(&config.night_start, "%H:%M").unwrap_or_else(|_| NaiveTime::from_hms_opt(22, 0, 0).unwrap());
    let night_end = NaiveTime::parse_from_str(&config.night_end, "%H:%M").unwrap_or_else(|_| NaiveTime::from_hms_opt(6, 0, 0).unwrap());

    // Check if current time is in night range
    let is_night = if night_start <= night_end {
        now >= night_start && now < night_end
    } else {
        // Crosses midnight (e.g. 22:00 - 06:00)
        now >= night_start || now < night_end
    };

    if is_night {
        if config.night_enabled {
            return Some("night".to_string());
        }
    } else {
        if config.day_enabled {
            return Some("day".to_string());
        }
    }
    None
}

async fn check_network_status(client: &ATClient) -> Result<bool> {
    // Check CREG
    let resp = send_command(client, "AT+CREG?\r\n").await?;
    if let Some(data) = resp.data {
        if data.contains("+CREG: 0,1") || data.contains("+CREG: 0,5") {
            return Ok(true);
        }
    }

    // Check CEREG
    let resp = send_command(client, "AT+CEREG?\r\n").await?;
    if let Some(data) = resp.data {
        if data.contains("+CEREG: 0,1") || data.contains("+CEREG: 0,5") {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn unlock_all(client: &ATClient, config: &ScheduleConfig) -> Result<()> {
    // Just reuse set_frequency_lock with a dummy "unlock" mode config or similar logic
    // But since set_frequency_lock reads from config based on mode string, we should probably construct a manual unlock
    
    info!("Unlocking all frequencies...");
    
    // Toggle airplane if configured
    if config.toggle_airplane {
        info!("Step 1: Enter airplane mode...");
        send_command(client, "AT+CFUN=0\r\n").await?;
        sleep(Duration::from_secs(2)).await;
    }

    // Unlock LTE
    info!("Step 2: Unlock LTE...");
    send_command(client, "AT^LTEFREQLOCK=0\r\n").await?;
    sleep(Duration::from_secs(1)).await;

    // Unlock NR
    info!("Step 3: Unlock NR...");
    send_command(client, "AT^NRFREQLOCK=0\r\n").await?;
    sleep(Duration::from_secs(1)).await;

    // Exit airplane mode
    if config.toggle_airplane {
        info!("Step 4: Exit airplane mode...");
        send_command(client, "AT+CFUN=1\r\n").await?;
        sleep(Duration::from_secs(5)).await;
    }
    
    Ok(())
}

async fn set_frequency_lock(client: &ATClient, config: &ScheduleConfig, mode: &str, switch_count: usize) -> Result<()> {
    info!("============================================================");
    info!("🔄 Switching to {} mode frequency lock (Count: {})", mode, switch_count);
    info!("============================================================");

    let (lte_type, lte_bands, lte_arfcns, lte_pcis, nr_type, nr_bands, nr_arfcns, nr_scs, nr_pcis) = if mode == "night" {
        (
            config.night_lte_type,
            &config.night_lte_bands,
            &config.night_lte_arfcns,
            &config.night_lte_pcis,
            config.night_nr_type,
            &config.night_nr_bands,
            &config.night_nr_arfcns,
            &config.night_nr_scs_types,
            &config.night_nr_pcis
        )
    } else {
        (
            config.day_lte_type,
            &config.day_lte_bands,
            &config.day_lte_arfcns,
            &config.day_lte_pcis,
            config.day_nr_type,
            &config.day_nr_bands,
            &config.day_nr_arfcns,
            &config.day_nr_scs_types,
            &config.day_nr_pcis
        )
    };

    // 1. Enter Airplane Mode
    if config.toggle_airplane {
        info!("Step 1: Enter airplane mode...");
        let resp = send_command(client, "AT+CFUN=0\r\n").await?;
        if resp.success {
            info!("✓ Entered airplane mode");
            sleep(Duration::from_secs(2)).await;
        } else {
            warn!("✗ Failed to enter airplane mode");
        }
    }

    // 2. Set LTE Lock
    if lte_type > 0 && !lte_bands.trim().is_empty() {
        let bands_list: Vec<&str> = lte_bands.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !bands_list.is_empty() {
            let cmd = build_lte_command(lte_type, &bands_list, lte_arfcns, lte_pcis);
            info!("Step 2: Set LTE Lock (Type: {})...", lte_type);
            info!("  Command: {}", cmd.trim());
            let resp = send_command(client, &cmd).await?;
            if resp.success {
                info!("✓ LTE Lock successful");
            } else {
                warn!("✗ LTE Lock failed: {:?}", resp.error);
            }
            sleep(Duration::from_secs(1)).await;
        }
    } else if config.unlock_lte {
        info!("Step 2: Unlock LTE...");
        send_command(client, "AT^LTEFREQLOCK=0\r\n").await?;
        sleep(Duration::from_secs(1)).await;
    }

    // 3. Set NR Lock
    if nr_type > 0 && !nr_bands.trim().is_empty() {
        let bands_list: Vec<&str> = nr_bands.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !bands_list.is_empty() {
            let cmd = build_nr_command(nr_type, &bands_list, nr_arfcns, nr_scs, nr_pcis);
            info!("Step 3: Set NR Lock (Type: {})...", nr_type);
            info!("  Command: {}", cmd.trim());
            let resp = send_command(client, &cmd).await?;
            if resp.success {
                info!("✓ NR Lock successful");
            } else {
                warn!("✗ NR Lock failed: {:?}", resp.error);
            }
            sleep(Duration::from_secs(1)).await;
        }
    } else if config.unlock_nr {
        info!("Step 3: Unlock NR...");
        send_command(client, "AT^NRFREQLOCK=0\r\n").await?;
        sleep(Duration::from_secs(1)).await;
    }

    // 4. Exit Airplane Mode
    if config.toggle_airplane {
        info!("Step 4: Exit airplane mode...");
        let resp = send_command(client, "AT+CFUN=1\r\n").await?;
        if resp.success {
            info!("✓ Exited airplane mode");
            sleep(Duration::from_secs(5)).await;
        } else {
            warn!("✗ Failed to exit airplane mode");
        }
    }

    info!("============================================================");
    info!("✓ Schedule frequency lock switch completed");
    info!("============================================================");

    Ok(())
}

async fn send_command(client: &ATClient, cmd: &str) -> Result<ATResponse> {
    let (tx, rx) = oneshot::channel();
    client.get_sender().send((cmd.to_string(), tx)).await.map_err(|_| anyhow!("Failed to send command"))?;
    match rx.await {
        Ok(resp) => Ok(resp),
        Err(_) => Err(anyhow!("Failed to receive response")),
    }
}

fn build_lte_command(lock_type: u8, bands: &[&str], arfcns: &str, pcis: &str) -> String {
    // Type 1: Frequency point lock (Band + ARFCN)
    // Type 2: Cell lock (Band + ARFCN + PCI)
    // Type 3: Band lock (Band only)
    
    if lock_type == 3 {
        // AT^LTEFREQLOCK=3,0,<count>,"<band1>,<band2>,..."
        return format!("AT^LTEFREQLOCK=3,0,{},\"{}\"\r\n", bands.len(), bands.join(","));
    } else if lock_type == 1 {
        let arfcn_list: Vec<&str> = arfcns.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if bands.len() != arfcn_list.len() {
            warn!("LTE Frequency Lock: Band count ({}) != ARFCN count ({}), unlocking", bands.len(), arfcn_list.len());
            return "AT^LTEFREQLOCK=0\r\n".to_string();
        }
        // AT^LTEFREQLOCK=1,0,<count>,"<band1>,...","<arfcn1>,..."
        return format!("AT^LTEFREQLOCK=1,0,{},\"{}\",\"{}\"\r\n", bands.len(), bands.join(","), arfcn_list.join(","));
    } else if lock_type == 2 {
        let arfcn_list: Vec<&str> = arfcns.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let pci_list: Vec<&str> = pcis.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        
        if bands.len() != arfcn_list.len() || arfcn_list.len() != pci_list.len() {
            warn!("LTE Cell Lock: Count mismatch (Band:{}, ARFCN:{}, PCI:{}), unlocking", bands.len(), arfcn_list.len(), pci_list.len());
            return "AT^LTEFREQLOCK=0\r\n".to_string();
        }
        // AT^LTEFREQLOCK=2,0,<count>,"<band1>,...","<arfcn1>,...","<pci1>,..."
        return format!("AT^LTEFREQLOCK=2,0,{},\"{}\",\"{}\",\"{}\"\r\n", bands.len(), bands.join(","), arfcn_list.join(","), pci_list.join(","));
    }

    "AT^LTEFREQLOCK=0\r\n".to_string()
}

fn build_nr_command(lock_type: u8, bands: &[&str], arfcns: &str, scs_types: &str, pcis: &str) -> String {
    // Type 1: Frequency point lock (Band + ARFCN)
    // Type 2: Cell lock (Band + ARFCN + SCS + PCI)
    // Type 3: Band lock (Band only)

    if lock_type == 3 {
        // AT^NRFREQLOCK=3,0,<count>,"<band1>,..."
        return format!("AT^NRFREQLOCK=3,0,{},\"{}\"\r\n", bands.len(), bands.join(","));
    } else if lock_type == 1 {
        let arfcn_list: Vec<&str> = arfcns.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if bands.len() != arfcn_list.len() {
            warn!("NR Frequency Lock: Band count ({}) != ARFCN count ({}), unlocking", bands.len(), arfcn_list.len());
            return "AT^NRFREQLOCK=0\r\n".to_string();
        }
        // AT^NRFREQLOCK=1,0,<count>,"<band1>,...","<arfcn1>,..."
        return format!("AT^NRFREQLOCK=1,0,{},\"{}\",\"{}\"\r\n", bands.len(), bands.join(","), arfcn_list.join(","));
    } else if lock_type == 2 {
        let arfcn_list: Vec<&str> = arfcns.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let scs_list: Vec<&str> = scs_types.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let pci_list: Vec<&str> = pcis.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

        if bands.len() != arfcn_list.len() || arfcn_list.len() != scs_list.len() || scs_list.len() != pci_list.len() {
            warn!("NR Cell Lock: Count mismatch (Band:{}, ARFCN:{}, SCS:{}, PCI:{}), unlocking", bands.len(), arfcn_list.len(), scs_list.len(), pci_list.len());
            return "AT^NRFREQLOCK=0\r\n".to_string();
        }
        // AT^NRFREQLOCK=2,0,<count>,"<band1>,...","<arfcn1>,...","<scs1>,...","<pci1>,..."
        return format!("AT^NRFREQLOCK=2,0,{},\"{}\",\"{}\",\"{}\",\"{}\"\r\n", bands.len(), bands.join(","), arfcn_list.join(","), scs_list.join(","), pci_list.join(","));
    }

    "AT^NRFREQLOCK=0\r\n".to_string()
}
