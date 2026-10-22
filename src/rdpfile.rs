//! Minimal .rdp parser: `name:type:value` lines into a lowercase-keyed map.

use std::collections::HashMap;

pub type RdpFile = HashMap<String, String>;

/// Feed .rdp files may be UTF-16LE with a BOM.
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)).into_owned()
    }
}

pub fn parse(text: &str) -> RdpFile {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(3, ':');
            let (name, _ty, value) = (parts.next()?, parts.next()?, parts.next()?);
            Some((name.to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utf16_with_colons_in_value() {
        let text = "full address:s:host.example:3389\r\nEnableRdsAadAuth:i:1\r\n";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let rdp = parse(&decode(&bytes));
        assert_eq!(rdp["full address"], "host.example:3389");
        assert_eq!(rdp["enablerdsaadauth"], "1");
    }
}
