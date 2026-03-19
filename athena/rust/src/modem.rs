use anyhow::Result;

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub trait Modem {
    fn initialize_network(&mut self, apn: &str) -> Result<()>;
    fn http_post(&mut self, url: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<HttpResponse>;
    fn http_get(&mut self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse>;
    fn battery_voltage(&mut self) -> Result<f32>;
    fn sleep(&mut self) -> Result<()>;
    fn wake(&mut self) -> Result<()>;
    fn is_connected(&self) -> bool;
}

pub fn parse_http_response(raw: &str) -> HttpResponse {
    let (header_section, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or((raw, ""));

    let mut lines = header_section.lines();

    let status = lines.next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    HttpResponse {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}

/// Binary-safe variant: splits headers and body at the \r\n\r\n boundary
/// without passing the body through a UTF-8 decoder.  Use this whenever
/// the response body may contain arbitrary bytes (e.g. firmware images).
pub fn parse_http_response_bytes(raw: &[u8]) -> HttpResponse {
    // Find the header/body separator
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n");
    let (header_bytes, body) = if let Some(pos) = sep {
        (&raw[..pos], raw[pos + 4..].to_vec())
    } else {
        (raw, Vec::new())
    };

    let header_str = String::from_utf8_lossy(header_bytes);
    let mut lines = header_str.lines();

    let status = lines.next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();

    HttpResponse { status, headers, body }
}
