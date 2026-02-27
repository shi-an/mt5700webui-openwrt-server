use anyhow::{anyhow, Result};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone};

#[derive(Debug, Clone)]
pub struct PartialInfo {
    pub reference: u16,
    pub parts_count: u8,
    pub part_number: u8,
}

#[derive(Debug, Clone)]
pub struct SmsData {
    pub sender: String,
    pub content: String,
    pub date: DateTime<FixedOffset>,
    pub partial_info: Option<PartialInfo>,
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(10 + (c - b'a')),
        b'A'..=b'F' => Some(10 + (c - b'A')),
        _ => None,
    }
}

fn hex_to_bytes(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if i + 1 >= bytes.len() {
            return Err(anyhow!("odd length hex string"));
        }
        let hi = hex_nibble(bytes[i]).ok_or_else(|| anyhow!("invalid hex"))?;
        let lo = hex_nibble(bytes[i + 1]).ok_or_else(|| anyhow!("invalid hex"))?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn gsm_alphabet() -> Vec<char> {
    let s = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ\x1bÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
    s.chars().collect()
}

pub fn decode_7bit(encoded_bytes: &[u8], length: usize) -> String {
    let table = gsm_alphabet();
    let mut result: Vec<u8> = Vec::with_capacity(length);
    let mut tmp: u16 = 0;
    let mut shift: u8 = 0;
    for &b in encoded_bytes {
        tmp |= (b as u16) << shift;
        shift = shift.wrapping_add(8);
        while shift >= 7 {
            result.push((tmp & 0x7F) as u8);
            tmp >>= 7;
            shift -= 7;
        }
    }
    if shift > 0 && result.len() < length {
        result.push((tmp & 0x7F) as u8);
    }
    let mut out = String::with_capacity(length);
    for &v in result.iter().take(length) {
        let ch = table.get(v as usize).copied().unwrap_or('?');
        out.push(ch);
    }
    out
}

pub fn decode_ucs2(encoded_bytes: &[u8]) -> String {
    use std::char::decode_utf16;
    let mut units: Vec<u16> = Vec::with_capacity(encoded_bytes.len() / 2);
    let mut i = 0usize;
    while i + 1 < encoded_bytes.len() {
        let hi = encoded_bytes[i] as u16;
        let lo = encoded_bytes[i + 1] as u16;
        units.push((hi << 8) | lo);
        i += 2;
    }
    let mut s = String::with_capacity(units.len());
    for r in decode_utf16(units.into_iter()) {
        match r {
            Ok(ch) => s.push(ch),
            Err(_) => s.push('?'),
        }
    }
    s
}

fn bcd_swap(byte: u8) -> u8 {
    ((byte & 0x0F) * 10) + (byte >> 4)
}

pub fn decode_timestamp(timestamp_bytes: &[u8]) -> DateTime<FixedOffset> {
    if timestamp_bytes.len() < 7 {
        let offset = FixedOffset::east_opt(0).unwrap();
        return offset.from_utc_datetime(&chrono::Utc::now().naive_utc());
    }
    let yy = bcd_swap(timestamp_bytes[0]) as i32;
    let mm = bcd_swap(timestamp_bytes[1]) as u32;
    let dd = bcd_swap(timestamp_bytes[2]) as u32;
    let hh = bcd_swap(timestamp_bytes[3]) as u32;
    let mi = bcd_swap(timestamp_bytes[4]) as u32;
    let ss = bcd_swap(timestamp_bytes[5]) as u32;
    let offset = FixedOffset::east_opt(0).unwrap();
    let year = 2000 + yy;
    let date = NaiveDate::from_ymd_opt(year, mm, dd);
    let time = NaiveTime::from_hms_opt(hh, mi, ss);
    match (date, time) {
        (Some(d), Some(t)) => offset
            .from_local_datetime(&d.and_time(t))
            .single()
            .unwrap_or_else(|| offset.from_utc_datetime(&chrono::Utc::now().naive_utc())),
        _ => offset.from_utc_datetime(&chrono::Utc::now().naive_utc()),
    }
}

pub fn decode_number(number_bytes: &[u8], number_length: usize) -> String {
    let mut number = String::with_capacity(number_length);
    for &byte in number_bytes {
        let d1 = byte & 0x0F;
        let d2 = byte >> 4;
        if d1 <= 9 {
            number.push(char::from(b'0' + d1));
        }
        if number.len() < number_length && d2 <= 9 {
            number.push(char::from(b'0' + d2));
        }
    }
    number
}

pub fn read_incoming_sms(pdu_hex: &str) -> Result<SmsData> {
    let pdu_bytes = hex_to_bytes(pdu_hex)?;
    let mut pos = 0usize;
    if pdu_bytes.is_empty() {
        return Err(anyhow!("empty pdu"));
    }
    let smsc_length = pdu_bytes[pos] as usize;
    pos = pos.checked_add(1 + smsc_length).ok_or_else(|| anyhow!("overflow"))?;
    if pos >= pdu_bytes.len() {
        return Err(anyhow!("invalid pdu"));
    }
    let pdu_type = pdu_bytes[pos];
    pos += 1;
    if pos + 2 > pdu_bytes.len() {
        return Err(anyhow!("invalid sender header"));
    }
    let sender_length = pdu_bytes[pos] as usize;
    pos += 1;
    let sender_type = pdu_bytes[pos];
    pos += 1;
    let sender_bytes_len = (sender_length + 1) / 2;
    if pos + sender_bytes_len > pdu_bytes.len() {
        return Err(anyhow!("invalid sender length"));
    }
    let sender_bytes = &pdu_bytes[pos..pos + sender_bytes_len];
    let mut sender = decode_number(sender_bytes, sender_length);
    if sender_type == 0x91 && !sender.starts_with('+') {
        sender.insert(0, '+');
    }
    pos += sender_bytes_len;
    if pos + 1 > pdu_bytes.len() {
        return Err(anyhow!("invalid pid"));
    }
    pos += 1;
    if pos + 1 > pdu_bytes.len() {
        return Err(anyhow!("invalid dcs"));
    }
    let dcs = pdu_bytes[pos];
    let is_ucs2 = (dcs & 0x0F) == 0x08;
    pos += 1;
    if pos + 7 > pdu_bytes.len() {
        return Err(anyhow!("invalid timestamp"));
    }
    let timestamp = decode_timestamp(&pdu_bytes[pos..pos + 7]);
    pos += 7;
    if pos + 1 > pdu_bytes.len() {
        return Err(anyhow!("invalid udl"));
    }
    let data_length = pdu_bytes[pos] as usize;
    pos += 1;
    if pos > pdu_bytes.len() {
        return Err(anyhow!("invalid data start"));
    }
    let data_bytes = &pdu_bytes[pos..];
    let mut udh_length = 0usize;
    let mut partial_info: Option<PartialInfo> = None;
    if (pdu_type & 0x40) != 0 && !data_bytes.is_empty() {
        udh_length = data_bytes[0] as usize + 1;
        if data_bytes.len() >= udh_length && data_bytes.len() >= 6 {
            let iei = data_bytes[1];
            if iei == 0x00 && data_bytes.len() >= 6 {
                let reference = data_bytes[3] as u16;
                let parts_count = data_bytes[4];
                let part_number = data_bytes[5];
                partial_info = Some(PartialInfo { reference, parts_count, part_number });
            } else if iei == 0x08 && data_bytes.len() >= 7 {
                let reference = ((data_bytes[3] as u16) << 8) | data_bytes[4] as u16;
                let parts_count = data_bytes[5];
                let part_number = data_bytes[6];
                partial_info = Some(PartialInfo { reference, parts_count, part_number });
            }
        }
    }
    let content_bytes = if udh_length <= data_bytes.len() {
        &data_bytes[udh_length..]
    } else {
        &[]
    };
    let content = if is_ucs2 {
        decode_ucs2(content_bytes)
    } else {
        decode_7bit(content_bytes, data_length)
    };
    let offset = FixedOffset::east_opt(0).unwrap();
    Ok(SmsData {
        sender,
        content,
        date: timestamp.with_timezone(&offset),
        partial_info,
    })
}
