//! Decoder for the AWS event-stream framing used by SelectObjectContent.

/// Returns the concatenated payloads of all `Records` events. An `error` event
/// becomes `Err`; input that contains no event-stream frames is returned as is.
pub fn parse_event_stream_records(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut frames = 0usize;
    let mut i = 0usize;
    while i + 16 <= data.len() {
        let total_len = be_u32(&data[i..]) as usize;
        let headers_len = be_u32(&data[i + 4..]) as usize;
        if total_len == 0 || i + total_len > data.len() || 12 + headers_len + 4 > total_len {
            break;
        }
        let headers_start = i + 12;
        let payload_start = headers_start + headers_len;
        let payload_end = i + total_len - 4;
        let headers = parse_headers(&data[headers_start..payload_start]);
        let payload = &data[payload_start..payload_end];
        frames += 1;

        let header = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        if header(":message-type") == Some("error") {
            return Err(format!(
                "select failed: {}: {}",
                header(":error-code").unwrap_or("unknown error"),
                header(":error-message").unwrap_or_default()
            ));
        }
        if header(":event-type") == Some("Records") {
            out.extend_from_slice(payload);
        }
        i += total_len;
    }
    if frames == 0 {
        return Ok(data.to_vec());
    }
    Ok(out)
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Decodes event-stream headers, keeping string values and skipping other types.
fn parse_headers(bytes: &[u8]) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    let mut j = 0usize;
    while j < bytes.len() {
        let name_len = bytes[j] as usize;
        j += 1;
        if j + name_len + 1 > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[j..j + name_len]).to_string();
        j += name_len;
        let value_type = bytes[j];
        j += 1;
        let value_len = match value_type {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            9 => 16,
            6 | 7 => {
                if j + 2 > bytes.len() {
                    break;
                }
                let len = u16::from_be_bytes([bytes[j], bytes[j + 1]]) as usize;
                j += 2;
                len
            }
            _ => break,
        };
        if j + value_len > bytes.len() {
            break;
        }
        if value_type == 7 {
            let value = String::from_utf8_lossy(&bytes[j..j + value_len]).to_string();
            headers.push((name, value));
        }
        j += value_len;
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::parse_event_stream_records;

    fn string_header(name: &str, value: &str) -> Vec<u8> {
        let mut h = vec![name.len() as u8];
        h.extend_from_slice(name.as_bytes());
        h.push(7);
        h.extend_from_slice(&(value.len() as u16).to_be_bytes());
        h.extend_from_slice(value.as_bytes());
        h
    }

    fn frame(headers: &[Vec<u8>], payload: &[u8]) -> Vec<u8> {
        let headers: Vec<u8> = headers.concat();
        let total_len = 12 + headers.len() + payload.len() + 4;
        let mut msg = Vec::new();
        msg.extend_from_slice(&(total_len as u32).to_be_bytes());
        msg.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        msg.extend_from_slice(&[0, 0, 0, 0]);
        msg.extend_from_slice(&headers);
        msg.extend_from_slice(payload);
        msg.extend_from_slice(&[0, 0, 0, 0]);
        msg
    }

    #[test]
    fn parse_event_stream_records_returns_payload_for_records_event() {
        let msg = frame(
            &[
                string_header(":message-type", "event"),
                string_header(":event-type", "Records"),
            ],
            b"row1,row2\n",
        );
        assert_eq!(
            parse_event_stream_records(&msg).expect("records"),
            b"row1,row2\n"
        );
    }

    #[test]
    fn non_record_events_produce_no_output() {
        let mut msg = frame(
            &[
                string_header(":message-type", "event"),
                string_header(":event-type", "Stats"),
            ],
            b"<Stats/>",
        );
        msg.extend(frame(
            &[
                string_header(":message-type", "event"),
                string_header(":event-type", "End"),
            ],
            b"",
        ));
        assert!(
            parse_event_stream_records(&msg)
                .expect("records")
                .is_empty()
        );
    }

    #[test]
    fn error_event_is_reported() {
        let msg = frame(
            &[
                string_header(":message-type", "error"),
                string_header(":error-code", "InvalidQuery"),
                string_header(":error-message", "bad sql"),
            ],
            b"",
        );
        assert_eq!(
            parse_event_stream_records(&msg),
            Err("select failed: InvalidQuery: bad sql".to_string())
        );
    }

    #[test]
    fn non_typed_headers_are_skipped() {
        let mut timestamp = vec![5u8];
        timestamp.extend_from_slice(b":date");
        timestamp.push(8);
        timestamp.extend_from_slice(&[0; 8]);
        let msg = frame(&[timestamp, string_header(":event-type", "Records")], b"x");
        assert_eq!(parse_event_stream_records(&msg).expect("records"), b"x");
    }

    #[test]
    fn plain_body_passes_through() {
        assert_eq!(
            parse_event_stream_records(b"a,b\n").expect("records"),
            b"a,b\n"
        );
    }
}
