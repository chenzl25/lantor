use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn percent_decode_utf8(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? << 4 | hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

pub(crate) fn compact_chars_middle(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_owned();
    }

    let head_len = limit.saturating_mul(2) / 3;
    let tail_len = limit.saturating_sub(head_len);
    let omitted = chars.len().saturating_sub(head_len + tail_len);
    let head = chars.iter().take(head_len).collect::<String>();
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(tail_len))
        .collect::<String>();
    format!("{head}\n\n[... Lantor omitted {omitted} chars to keep agent context bounded ...]\n\n{tail}")
}

fn decode_utf8_head(mut bytes: Vec<u8>, may_end_mid_character: bool) -> Result<String, String> {
    match String::from_utf8(bytes) {
        Ok(value) => Ok(value),
        Err(err) if may_end_mid_character && err.utf8_error().error_len().is_none() => {
            let valid_up_to = err.utf8_error().valid_up_to();
            bytes = err.into_bytes();
            bytes.truncate(valid_up_to);
            Ok(String::from_utf8(bytes).expect("validated UTF-8 prefix"))
        }
        Err(err) => Err(format!(
            "MEMORY.md is not valid UTF-8: {}",
            err.utf8_error()
        )),
    }
}

fn decode_utf8_tail(bytes: &[u8]) -> Result<String, String> {
    for skipped in 0..=bytes.len().min(3) {
        if let Ok(value) = std::str::from_utf8(&bytes[skipped..]) {
            return Ok(value.to_owned());
        }
    }
    Err("MEMORY.md is not valid UTF-8 near the end of the file".to_owned())
}

pub(crate) fn read_compact_memory_file(
    path: &Path,
    file_size: u64,
    char_limit: usize,
) -> Result<String, String> {
    const UTF8_MAX_BYTES_PER_CHAR: usize = 4;

    let edge_byte_budget = char_limit.saturating_mul(UTF8_MAX_BYTES_PER_CHAR);
    let full_read_limit = edge_byte_budget.saturating_mul(2);
    let mut file = fs::File::open(path).map_err(|err| err.to_string())?;

    if file_size <= full_read_limit as u64 {
        let mut bytes = Vec::with_capacity(file_size as usize);
        file.by_ref()
            .take(full_read_limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|err| err.to_string())?;
        if bytes.len() <= full_read_limit {
            let body = decode_utf8_head(bytes, false)?;
            return Ok(compact_chars_middle(body.trim(), char_limit));
        }
    }

    file.seek(SeekFrom::Start(0))
        .map_err(|err| err.to_string())?;
    let mut head_bytes = Vec::with_capacity(edge_byte_budget);
    file.by_ref()
        .take(edge_byte_budget as u64)
        .read_to_end(&mut head_bytes)
        .map_err(|err| err.to_string())?;

    let tail_start = file_size.saturating_sub(edge_byte_budget as u64);
    file.seek(SeekFrom::Start(tail_start))
        .map_err(|err| err.to_string())?;
    let mut tail_bytes = Vec::with_capacity(edge_byte_budget);
    file.take(edge_byte_budget as u64)
        .read_to_end(&mut tail_bytes)
        .map_err(|err| err.to_string())?;

    let head = decode_utf8_head(head_bytes, true)?;
    let tail = decode_utf8_tail(&tail_bytes)?;
    let head_len = char_limit.saturating_mul(2) / 3;
    let tail_len = char_limit.saturating_sub(head_len);
    let compact_head = head.trim_start().chars().take(head_len).collect::<String>();
    let compact_tail = tail
        .trim_end()
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    Ok(format!(
        "{compact_head}\n\n[... Lantor omitted the middle of this {file_size}-byte MEMORY.md ...]\n\n{compact_tail}"
    ))
}
