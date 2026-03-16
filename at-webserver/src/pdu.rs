use chrono::{DateTime, Local, TimeZone, NaiveDate};
use anyhow::{Result, Context};

// GSM 7-bit default alphabet
const GSM_7BIT_ALPHABET: [char; 128] = [
    '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', 'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å', 'å',
    'Δ', '_', 'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', 'Σ', 'Θ', 'Ξ', '\u{001b}', 'Æ', 'æ', 'ß', 'É',
    ' ', '!', '"', '#', '¤', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'Ä', 'Ö', 'Ñ', 'Ü', '§',
    '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü', 'à'
];

#[derive(Debug, Clone)]
pub struct PartialInfo {
    pub reference: u8,
    pub parts_count: u8,
    pub part_number: u8,
}

#[derive(Debug, Clone)]
pub struct SmsData {
    pub sender: String,
    pub content: String,
    pub date: DateTime<Local>,
    pub partial_info: Option<PartialInfo>,
}

fn decode_7bit(encoded_bytes: &[u8], length: usize) -> String {
    let mut result = Vec::new();
    let mut shift = 0;
    let mut tmp = 0u16;

    for &byte in encoded_bytes {
        tmp |= (byte as u16) << shift;
        shift += 8;

        while shift >= 7 {
            result.push((tmp & 0x7F) as usize);
            tmp >>= 7;
            shift -= 7;
        }
    }

    if shift > 0 && result.len() < length {
        result.push((tmp & 0x7F) as usize);
    }

    result.iter()
        .take(length)
        .map(|&b| if b < GSM_7BIT_ALPHABET.len() { GSM_7BIT_ALPHABET[b] } else { '?' })
        .collect()
}

fn decode_ucs2(encoded_bytes: &[u8]) -> String {
    let u16_vec: Vec<u16> = encoded_bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();
    
    String::from_utf16(&u16_vec).unwrap_or_else(|_| "?".repeat(encoded_bytes.len() / 2))
}

fn decode_timestamp(timestamp_bytes: &[u8]) -> DateTime<Local> {
    if timestamp_bytes.len() < 7 {
        return Local::now();
    }

    let swap_nibbles = |b: u8| -> u8 {
        ((b & 0x0F) * 10) + (b >> 4)
    };

    let year = 2000 + swap_nibbles(timestamp_bytes[0]) as i32;
    let month = swap_nibbles(timestamp_bytes[1]) as u32;
    let day = swap_nibbles(timestamp_bytes[2]) as u32;
    let hour = swap_nibbles(timestamp_bytes[3]) as u32;
    let minute = swap_nibbles(timestamp_bytes[4]) as u32;
    let second = swap_nibbles(timestamp_bytes[5]) as u32;
    
    match NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, second)) 
    {
        Some(dt) => Local.from_local_datetime(&dt).unwrap(), // Best effort
        None => Local::now(),
    }
}

fn decode_number(number_bytes: &[u8], number_length: usize) -> String {
    let mut number = String::new();
    for &byte in number_bytes {
        let digit1 = byte & 0x0F;
        let digit2 = byte >> 4;
        
        if digit1 <= 9 {
            number.push(char::from_digit(digit1 as u32, 10).unwrap());
        }
        if number.len() < number_length && digit2 <= 9 {
            number.push(char::from_digit(digit2 as u32, 10).unwrap());
        }
    }
    number
}

pub fn read_incoming_sms(pdu_hex: &str) -> Result<SmsData> {
    let pdu_bytes = hex::decode(pdu_hex).context("Invalid hex string")?;
    let mut pos = 0;

    // Skip SMSC info
    if pos >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    let smsc_length = pdu_bytes[pos] as usize;
    pos += 1 + smsc_length;

    // PDU type
    if pos >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    let pdu_type = pdu_bytes[pos];
    pos += 1;

    // Sender number length and type
    if pos + 1 >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    let sender_length = pdu_bytes[pos] as usize;
    pos += 1;
    let _sender_type = pdu_bytes[pos];
    pos += 1;

    // Decode sender number
    let sender_bytes_len = (sender_length + 1) / 2;
    if pos + sender_bytes_len > pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short for sender")); }
    let sender = decode_number(&pdu_bytes[pos..pos + sender_bytes_len], sender_length);
    pos += sender_bytes_len;

    // Skip protocol identifier
    if pos >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    pos += 1;

    // Data Coding Scheme
    if pos >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    let dcs = pdu_bytes[pos];
    let is_ucs2 = (dcs & 0x0F) == 0x08;
    pos += 1;

    // Timestamp
    if pos + 7 > pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short for timestamp")); }
    let timestamp = decode_timestamp(&pdu_bytes[pos..pos + 7]);
    pos += 7;

    // User data length
    if pos >= pdu_bytes.len() { return Err(anyhow::anyhow!("PDU too short")); }
    let data_length = pdu_bytes[pos] as usize;
    pos += 1;
    
    let data_bytes = &pdu_bytes[pos..];
    
    // Check for UDH
    let mut udh_length = 0;
    let mut partial_info = None;

    if (pdu_type & 0x40) != 0 {
        if data_bytes.is_empty() { return Err(anyhow::anyhow!("PDU too short for UDH")); }
        udh_length = (data_bytes[0] + 1) as usize;
        
        if udh_length >= 6 && data_bytes.len() >= udh_length {
             let iei = data_bytes[1];
             if iei == 0x00 || iei == 0x08 {
                 if udh_length >= 6 {
                     let ref_num = data_bytes[3];
                     let total = data_bytes[4];
                     let seq = data_bytes[5];
                     partial_info = Some(PartialInfo {
                         reference: ref_num,
                         parts_count: total,
                         part_number: seq,
                     });
                 }
             }
        }
    }

    let content_bytes = if data_bytes.len() >= udh_length {
        &data_bytes[udh_length..]
    } else {
        &[]
    };

    let content = if is_ucs2 {
        decode_ucs2(content_bytes)
    } else {
        decode_7bit(content_bytes, data_length)
    };

    Ok(SmsData {
        sender,
        content,
        date: timestamp,
        partial_info,
    })
}
