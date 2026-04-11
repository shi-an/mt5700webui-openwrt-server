use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, TimeZone};

// GSM 7-bit default alphabet
const GSM_7BIT_ALPHABET: [char; 128] = [
    '@', '\u{00A3}', '$', '\u{00A5}', '\u{00E8}', '\u{00E9}', '\u{00F9}', '\u{00EC}',
    '\u{00F2}', '\u{00C7}', '\n', '\u{00D8}', '\u{00F8}', '\r', '\u{00C5}', '\u{00E5}',
    '\u{0394}', '_', '\u{03A6}', '\u{0393}', '\u{039B}', '\u{03A9}', '\u{03A0}', '\u{03A8}',
    '\u{03A3}', '\u{0398}', '\u{039E}', '\u{001B}', '\u{00C6}', '\u{00E6}', '\u{00DF}', '\u{00C9}',
    ' ', '!', '"', '#', '\u{00A4}', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    '\u{00A1}', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '\u{00C4}', '\u{00D6}', '\u{00D1}',
    '\u{00DC}', '\u{00A7}', '\u{00BF}', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k',
    'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '\u{00E4}',
    '\u{00F6}', '\u{00F1}', '\u{00FC}', '\u{00E0}',
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

#[derive(Debug, Clone)]
pub struct MmsNotification {
    pub sender: String,
    pub content_location: Option<String>,
    pub transaction_id: Option<String>,
    pub content_type: Option<String>,
    pub raw_hex: String,
    pub date: DateTime<Local>,
}

#[derive(Debug, Clone)]
pub enum IncomingMessage {
    Sms(SmsData),
    MmsNotification(MmsNotification),
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

    result
        .iter()
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

    let swap_nibbles = |b: u8| -> u8 { ((b & 0x0F) * 10) + (b >> 4) };

    let year = 2000 + swap_nibbles(timestamp_bytes[0]) as i32;
    let month = swap_nibbles(timestamp_bytes[1]) as u32;
    let day = swap_nibbles(timestamp_bytes[2]) as u32;
    let hour = swap_nibbles(timestamp_bytes[3]) as u32;
    let minute = swap_nibbles(timestamp_bytes[4]) as u32;
    let second = swap_nibbles(timestamp_bytes[5]) as u32;

    match NaiveDate::from_ymd_opt(year, month, day).and_then(|d| d.and_hms_opt(hour, minute, second)) {
        Some(dt) => Local.from_local_datetime(&dt).unwrap(),
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

fn extract_ascii_field(bytes: &[u8], needle: &str) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let pos = text.find(needle)?;
    let rest = &text[pos + needle.len()..];
    let end = rest.find('\0').unwrap_or(rest.len());
    let value = rest[..end].trim_matches(char::from(0)).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn try_decode_mms_notification(
    sender: &str,
    timestamp: DateTime<Local>,
    pdu_hex: &str,
    data_bytes: &[u8],
) -> Option<MmsNotification> {
    let text = String::from_utf8_lossy(data_bytes).to_lowercase();
    let is_mms = text.contains("application/vnd.wap.mms-message")
        || text.contains("m-notification-ind")
        || text.contains("x-mms-message-type")
        || text.contains("content-location");

    if !is_mms {
        return None;
    }

    Some(MmsNotification {
        sender: sender.to_string(),
        content_location: extract_ascii_field(data_bytes, "Content-Location:")
            .or_else(|| extract_ascii_field(data_bytes, "content-location:"))
            .or_else(|| extract_ascii_field(data_bytes, "http://" ).map(|v| format!("http://{}", v)))
            .or_else(|| extract_ascii_field(data_bytes, "https://" ).map(|v| format!("https://{}", v))),
        transaction_id: extract_ascii_field(data_bytes, "X-Mms-Transaction-ID:")
            .or_else(|| extract_ascii_field(data_bytes, "x-mms-transaction-id:")),
        content_type: extract_ascii_field(data_bytes, "Content-Type:")
            .or_else(|| extract_ascii_field(data_bytes, "content-type:"))
            .or_else(|| extract_ascii_field(data_bytes, "application/vnd.wap.mms-message")),
        raw_hex: pdu_hex.to_string(),
        date: timestamp,
    })
}

pub fn read_incoming_sms(pdu_hex: &str) -> Result<IncomingMessage> {
    let pdu_bytes = hex::decode(pdu_hex).context("Invalid hex string")?;
    let mut pos = 0;

    if pos >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    let smsc_length = pdu_bytes[pos] as usize;
    pos += 1 + smsc_length;

    if pos >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    let pdu_type = pdu_bytes[pos];
    pos += 1;

    if pos + 1 >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    let sender_length = pdu_bytes[pos] as usize;
    pos += 1;
    let _sender_type = pdu_bytes[pos];
    pos += 1;

    let sender_bytes_len = (sender_length + 1) / 2;
    if pos + sender_bytes_len > pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short for sender"));
    }
    let sender = decode_number(&pdu_bytes[pos..pos + sender_bytes_len], sender_length);
    pos += sender_bytes_len;

    if pos >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    pos += 1;

    if pos >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    let dcs = pdu_bytes[pos];
    let is_ucs2 = (dcs & 0x0F) == 0x08;
    pos += 1;

    if pos + 7 > pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short for timestamp"));
    }
    let timestamp = decode_timestamp(&pdu_bytes[pos..pos + 7]);
    pos += 7;

    if pos >= pdu_bytes.len() {
        return Err(anyhow::anyhow!("PDU too short"));
    }
    let data_length = pdu_bytes[pos] as usize;
    pos += 1;

    let data_bytes = &pdu_bytes[pos..];

    let mut udh_length = 0usize;
    let mut partial_info = None;

    if (pdu_type & 0x40) != 0 {
        if data_bytes.is_empty() {
            return Err(anyhow::anyhow!("PDU too short for UDH"));
        }
        udh_length = (data_bytes[0] + 1) as usize;

        if udh_length >= 6 && data_bytes.len() >= udh_length {
            let iei = data_bytes[1];
            if iei == 0x00 || iei == 0x08 {
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

    if let Some(mms) = try_decode_mms_notification(&sender, timestamp, pdu_hex, data_bytes) {
        return Ok(IncomingMessage::MmsNotification(mms));
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

    Ok(IncomingMessage::Sms(SmsData {
        sender,
        content,
        date: timestamp,
        partial_info,
    }))
}
