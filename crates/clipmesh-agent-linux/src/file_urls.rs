//! Native text/uri-list selections. These functions must only receive a
//! clipboard offer explicitly advertising that MIME type, never plain text.
use crate::LinuxAdapterError;
use std::path::PathBuf;

pub fn decode(bytes: &[u8]) -> Result<Vec<PathBuf>, LinuxAdapterError> {
    let invalid = || LinuxAdapterError::AdapterUnavailable;
    if bytes.len() > 256 * 1024 {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut paths = Vec::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let path = line
            .strip_prefix("file://localhost/")
            .map(|tail| format!("/{tail}"))
            .or_else(|| line.strip_prefix("file://").map(str::to_owned))
            .ok_or_else(invalid)?;
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#']) {
            return Err(invalid());
        }
        let mut decoded = Vec::new();
        let mut bytes = path.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                let high = bytes
                    .next()
                    .and_then(|b| (b as char).to_digit(16))
                    .ok_or_else(invalid)?;
                let low = bytes
                    .next()
                    .and_then(|b| (b as char).to_digit(16))
                    .ok_or_else(invalid)?;
                decoded.push((high * 16 + low) as u8);
            } else {
                decoded.push(byte);
            }
        }
        let decoded = String::from_utf8(decoded).map_err(|_| invalid())?;
        if decoded.chars().any(char::is_control) {
            return Err(invalid());
        }
        paths.push(PathBuf::from(decoded));
        if paths.len() > 32 {
            return Err(invalid());
        }
    }
    if paths.is_empty() {
        return Err(invalid());
    }
    Ok(paths)
}

pub fn encode(paths: &[PathBuf]) -> Result<Vec<u8>, LinuxAdapterError> {
    if paths.is_empty() || paths.len() > 32 {
        return Err(LinuxAdapterError::AdapterUnavailable);
    }
    let mut result = String::new();
    for path in paths {
        let text = path
            .to_str()
            .filter(|_| path.is_absolute())
            .ok_or(LinuxAdapterError::AdapterUnavailable)?;
        if text.chars().any(char::is_control) {
            return Err(LinuxAdapterError::AdapterUnavailable);
        }
        result.push_str("file://");
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                result.push(byte as char);
            } else {
                result.push_str(&format!("%{byte:02X}"));
            }
        }
        result.push_str("\r\n");
    }
    if result.len() > 256 * 1024 {
        return Err(LinuxAdapterError::AdapterUnavailable);
    }
    Ok(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_file_urls_preserve_names_and_reject_remote_or_ambiguous_urls() {
        let paths = vec![
            PathBuf::from("/tmp/a file #1 %.zip"),
            PathBuf::from("/tmp/猫.png"),
        ];
        assert_eq!(decode(&encode(&paths).unwrap()).unwrap(), paths);
        assert_eq!(
            decode(b"# files\r\nfile://localhost/tmp/a%20b\r\n").unwrap(),
            vec![PathBuf::from("/tmp/a b")]
        );
        for value in [
            "/tmp/file",
            "https://host/file",
            "file://remote/tmp/file",
            "file:///tmp/%",
            "file:///tmp/%00",
            "file:///tmp/file?query",
            "file:///tmp/file#fragment",
            "file:///tmp/%ff",
        ] {
            assert!(decode(value.as_bytes()).is_err(), "{value}");
        }
        assert!(decode("file:///tmp/a\n".repeat(33).as_bytes()).is_err());
    }
}
