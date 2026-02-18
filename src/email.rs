use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::Serialize;
use thiserror::Error;

use crate::config::EmailMode;

#[derive(Debug, Clone)]
pub struct EmailAccountCredentials {
    pub email: String,
    pub app_password: String,
    pub imap_host: String,
    pub imap_port: u16,
}

#[derive(Debug, Clone)]
pub struct EmailPolicy {
    pub mode: EmailMode,
    pub sender_rules: Vec<String>,
    pub default_max_results: u32,
}

impl EmailPolicy {
    pub fn sender_visible(&self, from_header: &str) -> bool {
        let Some(sender) = extract_sender_address(from_header) else {
            return false;
        };
        let matches_rule = self
            .sender_rules
            .iter()
            .any(|rule| sender_matches_rule(&sender, rule));
        match self.mode {
            EmailMode::Allowlist => matches_rule,
            EmailMode::Denylist => !matches_rule,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EmailListMessagesRequest {
    pub folder: String,
    pub max_results: u32,
    pub unread_only: bool,
    pub from_contains: Option<String>,
    pub subject_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailMessageMetadata {
    pub id: String,
    pub thread_id: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub snippet: Option<String>,
    pub internal_date_ms: Option<i64>,
    pub label_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EmailListMessagesResponse {
    pub messages: Vec<EmailMessageMetadata>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EmailGetMessageRequest {
    pub folder: String,
    pub message_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailMessageContent {
    pub metadata: EmailMessageMetadata,
    pub headers: BTreeMap<String, String>,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
}

#[derive(Debug, Error)]
pub enum EmailServiceError {
    #[error("failed to build email client: {0}")]
    ClientSetup(String),
    #[error("email authentication failed: {0}")]
    Authentication(String),
    #[error("email API error: {0}")]
    Api(String),
    #[error("email message not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone)]
pub enum EmailService {
    Imap(ImapEmailService),
    #[cfg(test)]
    Mock(MockEmailService),
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum MockEmailService {
    Disabled,
    Static(EmailListMessagesResponse),
    StaticContent(BTreeMap<String, EmailMessageContent>),
}

impl EmailService {
    pub async fn list_message_metadata(
        &self,
        _account_key: &str,
        credentials: &EmailAccountCredentials,
        request: &EmailListMessagesRequest,
    ) -> Result<EmailListMessagesResponse, EmailServiceError> {
        match self {
            EmailService::Imap(service) => {
                service.list_message_metadata(credentials, request).await
            }
            #[cfg(test)]
            EmailService::Mock(mock) => match mock {
                MockEmailService::Disabled => {
                    Err(EmailServiceError::Api("email service disabled".to_string()))
                }
                MockEmailService::Static(response) => Ok(response.clone()),
                MockEmailService::StaticContent(_) => Ok(EmailListMessagesResponse {
                    messages: Vec::new(),
                    next_page_token: None,
                }),
            },
        }
    }

    pub async fn get_message_content(
        &self,
        _account_key: &str,
        credentials: &EmailAccountCredentials,
        request: &EmailGetMessageRequest,
    ) -> Result<EmailMessageContent, EmailServiceError> {
        match self {
            EmailService::Imap(service) => service.get_message_content(credentials, request).await,
            #[cfg(test)]
            EmailService::Mock(mock) => match mock {
                MockEmailService::Disabled => {
                    Err(EmailServiceError::Api("email service disabled".to_string()))
                }
                MockEmailService::Static(_) => {
                    Err(EmailServiceError::NotFound(request.message_id.clone()))
                }
                MockEmailService::StaticContent(contents) => contents
                    .get(&request.message_id)
                    .cloned()
                    .ok_or_else(|| EmailServiceError::NotFound(request.message_id.clone())),
            },
        }
    }

    #[cfg(test)]
    pub fn mock_disabled() -> Self {
        Self::Mock(MockEmailService::Disabled)
    }

    #[cfg(test)]
    pub fn mock_static(response: EmailListMessagesResponse) -> Self {
        Self::Mock(MockEmailService::Static(response))
    }

    #[cfg(test)]
    pub fn mock_static_content(contents: BTreeMap<String, EmailMessageContent>) -> Self {
        Self::Mock(MockEmailService::StaticContent(contents))
    }
}

#[derive(Debug, Clone)]
pub struct ImapEmailService {
    timeout: Duration,
}

impl Default for ImapEmailService {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(20),
        }
    }
}

impl ImapEmailService {
    pub async fn list_message_metadata(
        &self,
        credentials: &EmailAccountCredentials,
        request: &EmailListMessagesRequest,
    ) -> Result<EmailListMessagesResponse, EmailServiceError> {
        let credentials = credentials.clone();
        let request = request.clone();
        let timeout = self.timeout;

        tokio::task::spawn_blocking(move || {
            list_messages_imap_blocking(&credentials, &request, timeout)
        })
        .await
        .map_err(|e| EmailServiceError::Api(format!("imap worker join error: {e}")))?
    }

    pub fn validate_credentials_blocking(
        credentials: &EmailAccountCredentials,
        timeout: Duration,
    ) -> Result<(), EmailServiceError> {
        validate_imap_login_blocking(credentials, timeout)
    }

    pub async fn get_message_content(
        &self,
        credentials: &EmailAccountCredentials,
        request: &EmailGetMessageRequest,
    ) -> Result<EmailMessageContent, EmailServiceError> {
        let credentials = credentials.clone();
        let request = request.clone();
        let timeout = self.timeout;

        tokio::task::spawn_blocking(move || {
            get_message_content_imap_blocking(&credentials, &request, timeout)
        })
        .await
        .map_err(|e| EmailServiceError::Api(format!("imap worker join error: {e}")))?
    }
}

pub fn validate_imap_login_blocking(
    credentials: &EmailAccountCredentials,
    timeout: Duration,
) -> Result<(), EmailServiceError> {
    let mut session = connect_and_login(credentials, timeout)?;
    session.select_mailbox("INBOX")?;
    session.logout();
    Ok(())
}

fn list_messages_imap_blocking(
    credentials: &EmailAccountCredentials,
    request: &EmailListMessagesRequest,
    timeout: Duration,
) -> Result<EmailListMessagesResponse, EmailServiceError> {
    let mut session = connect_and_login(credentials, timeout)?;
    session.select_mailbox(&request.folder)?;

    let mut uids = session.search_uids(request)?;
    // Most IMAP servers return ascending UID order. Reverse so newest come first.
    uids.sort_unstable();
    uids.reverse();

    let limit = usize::try_from(request.max_results).unwrap_or(100);
    if uids.len() > limit {
        uids.truncate(limit);
    }

    let mut messages = Vec::new();
    for uid in uids {
        if let Some(message) = session.fetch_message_metadata(uid)? {
            messages.push(message);
        }
    }

    session.logout();

    Ok(EmailListMessagesResponse {
        messages,
        next_page_token: None,
    })
}

fn get_message_content_imap_blocking(
    credentials: &EmailAccountCredentials,
    request: &EmailGetMessageRequest,
    timeout: Duration,
) -> Result<EmailMessageContent, EmailServiceError> {
    let mut session = connect_and_login(credentials, timeout)?;
    session.select_mailbox(&request.folder)?;

    let uid = request
        .message_id
        .trim()
        .parse::<u64>()
        .map_err(|_| EmailServiceError::Api("invalid message id".to_string()))?;
    let content = session
        .fetch_message_content(uid)?
        .ok_or_else(|| EmailServiceError::NotFound(request.message_id.clone()))?;

    session.logout();
    Ok(content)
}

struct ImapCommandOutput {
    lines: Vec<String>,
    literals: Vec<Vec<u8>>,
}

struct BlockingImapSession {
    stream: BufReader<StreamOwned<ClientConnection, TcpStream>>,
    next_tag: u32,
}

impl BlockingImapSession {
    fn new(stream: StreamOwned<ClientConnection, TcpStream>) -> Self {
        Self {
            stream: BufReader::new(stream),
            next_tag: 1,
        }
    }

    fn expect_greeting(&mut self) -> Result<(), EmailServiceError> {
        let line = self.read_line()?;
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("* OK") || upper.starts_with("* PREAUTH") {
            Ok(())
        } else {
            Err(EmailServiceError::ClientSetup(format!(
                "unexpected IMAP greeting: {line}"
            )))
        }
    }

    fn login(&mut self, username: &str, password: &str) -> Result<(), EmailServiceError> {
        let command = format!("LOGIN {} {}", imap_quote(username), imap_quote(password));
        self.run_command(&command)
            .map(|_| ())
            .map_err(|error| match error {
                EmailServiceError::Api(message) => EmailServiceError::Authentication(message),
                other => other,
            })
    }

    fn logout(&mut self) {
        let _ = self.run_command("LOGOUT");
    }

    fn select_mailbox(&mut self, folder: &str) -> Result<(), EmailServiceError> {
        let folder = folder.trim();
        let mailbox = if folder.is_empty() { "INBOX" } else { folder };
        self.run_command(&format!("SELECT {}", imap_quote(mailbox)))
            .map(|_| ())
    }

    fn search_uids(
        &mut self,
        request: &EmailListMessagesRequest,
    ) -> Result<Vec<u64>, EmailServiceError> {
        let mut criteria = vec!["ALL".to_string()];

        if request.unread_only {
            criteria.push("UNSEEN".to_string());
        }
        if let Some(from) = request
            .from_contains
            .as_deref()
            .and_then(normalize_search_term)
        {
            criteria.push(format!("FROM {}", imap_quote(&from)));
        }
        if let Some(subject) = request
            .subject_contains
            .as_deref()
            .and_then(normalize_search_term)
        {
            criteria.push(format!("SUBJECT {}", imap_quote(&subject)));
        }

        let output = self.run_command(&format!("UID SEARCH {}", criteria.join(" ")))?;

        let mut uids = Vec::new();
        for line in output.lines {
            let Some(rest) = line.strip_prefix("* SEARCH") else {
                continue;
            };
            for token in rest.split_whitespace() {
                if let Ok(uid) = token.parse::<u64>() {
                    uids.push(uid);
                }
            }
        }

        Ok(uids)
    }

    fn fetch_message_metadata(
        &mut self,
        uid: u64,
    ) -> Result<Option<EmailMessageMetadata>, EmailServiceError> {
        let output = self.run_command(&format!(
            "UID FETCH {uid} (UID FLAGS INTERNALDATE X-GM-THRID BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)])"
        ))?;

        let Some(fetch_line) = output
            .lines
            .iter()
            .find(|line| line.starts_with("* ") && line.contains(" FETCH ("))
        else {
            return Ok(None);
        };

        let parsed_uid = capture_numeric_token(fetch_line, "UID")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(uid);
        let thread_id = capture_numeric_token(fetch_line, "X-GM-THRID");
        let flags = capture_parenthesized_token(fetch_line, "FLAGS")
            .map(|value| parse_flags(&value))
            .unwrap_or_default();
        let internal_date = capture_quoted_token(fetch_line, "INTERNALDATE");

        let headers = output
            .literals
            .first()
            .map(|literal| parse_headers(literal))
            .unwrap_or_default();

        let from = headers.get("from").cloned();
        let subject = headers.get("subject").cloned();
        let date = headers.get("date").cloned().or(internal_date);

        Ok(Some(EmailMessageMetadata {
            id: parsed_uid.to_string(),
            thread_id,
            from,
            subject,
            date,
            snippet: None,
            internal_date_ms: None,
            label_ids: flags,
        }))
    }

    fn fetch_message_content(
        &mut self,
        uid: u64,
    ) -> Result<Option<EmailMessageContent>, EmailServiceError> {
        let output = self.run_command(&format!(
            "UID FETCH {uid} (UID FLAGS INTERNALDATE X-GM-THRID BODY.PEEK[])"
        ))?;

        let Some(fetch_line) = output
            .lines
            .iter()
            .find(|line| line.starts_with("* ") && line.contains(" FETCH ("))
        else {
            return Ok(None);
        };

        let literal = output.literals.first().ok_or_else(|| {
            EmailServiceError::Api("IMAP FETCH response missing message body literal".to_string())
        })?;

        let parsed_uid = capture_numeric_token(fetch_line, "UID")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(uid);
        let thread_id = capture_numeric_token(fetch_line, "X-GM-THRID");
        let flags = capture_parenthesized_token(fetch_line, "FLAGS")
            .map(|value| parse_flags(&value))
            .unwrap_or_default();
        let internal_date = capture_quoted_token(fetch_line, "INTERNALDATE");

        let (headers, body) = split_headers_and_body(literal);
        let from = headers.get("from").cloned();
        let subject = headers.get("subject").cloned();
        let date = headers.get("date").cloned().or(internal_date);
        let (text_body, html_body) = extract_text_and_html_bodies(&headers, body);

        Ok(Some(EmailMessageContent {
            metadata: EmailMessageMetadata {
                id: parsed_uid.to_string(),
                thread_id,
                from,
                subject,
                date,
                snippet: text_body
                    .as_deref()
                    .map(|value| value.chars().take(240).collect::<String>())
                    .filter(|value| !value.trim().is_empty()),
                internal_date_ms: None,
                label_ids: flags,
            },
            headers,
            text_body,
            html_body,
        }))
    }

    fn run_command(&mut self, command: &str) -> Result<ImapCommandOutput, EmailServiceError> {
        let tag = format!("A{:04}", self.next_tag);
        self.next_tag = self.next_tag.saturating_add(1);

        let command_line = format!("{tag} {command}\r\n");
        self.stream
            .get_mut()
            .write_all(command_line.as_bytes())
            .map_err(|e| EmailServiceError::Api(format!("failed to write IMAP command: {e}")))?;
        self.stream
            .get_mut()
            .flush()
            .map_err(|e| EmailServiceError::Api(format!("failed to flush IMAP command: {e}")))?;

        let mut lines = Vec::new();
        let mut literals = Vec::new();

        loop {
            let line = self.read_line()?;
            let literal_size = parse_literal_size(&line);
            lines.push(line.clone());

            if let Some(size) = literal_size {
                let mut literal = vec![0u8; size];
                self.stream.read_exact(&mut literal).map_err(|e| {
                    EmailServiceError::Api(format!("failed to read IMAP literal: {e}"))
                })?;
                literals.push(literal);
            }

            if line.starts_with(&tag) {
                let upper = line.to_ascii_uppercase();
                if !upper.contains(" OK") {
                    return Err(EmailServiceError::Api(format!(
                        "IMAP command '{command}' failed: {line}"
                    )));
                }
                break;
            }
        }

        Ok(ImapCommandOutput { lines, literals })
    }

    fn read_line(&mut self) -> Result<String, EmailServiceError> {
        let mut line = String::new();
        let read = self.stream.read_line(&mut line).map_err(|e| {
            EmailServiceError::Api(format!("failed to read IMAP response line: {e}"))
        })?;

        if read == 0 {
            return Err(EmailServiceError::Api(
                "unexpected EOF while reading IMAP response".to_string(),
            ));
        }

        while matches!(line.chars().last(), Some('\n' | '\r')) {
            line.pop();
        }
        Ok(line)
    }
}

fn connect_and_login(
    credentials: &EmailAccountCredentials,
    timeout: Duration,
) -> Result<BlockingImapSession, EmailServiceError> {
    let host = credentials.imap_host.trim();
    if host.is_empty() {
        return Err(EmailServiceError::ClientSetup(
            "IMAP host cannot be empty".to_string(),
        ));
    }
    if credentials.imap_port == 0 {
        return Err(EmailServiceError::ClientSetup(
            "IMAP port must be greater than 0".to_string(),
        ));
    }

    let mut root_store = RootCertStore::empty();
    let native_certs = rustls_native_certs::load_native_certs();
    for cert in native_certs.certs {
        root_store.add(cert).map_err(|e| {
            EmailServiceError::ClientSetup(format!("failed to add native root certificate: {e}"))
        })?;
    }
    if root_store.is_empty() {
        if native_certs.errors.is_empty() {
            return Err(EmailServiceError::ClientSetup(
                "no native root certificates found".to_string(),
            ));
        }
        return Err(EmailServiceError::ClientSetup(format!(
            "failed to load native root certificates: {:?}",
            native_certs.errors
        )));
    }

    let client_config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| EmailServiceError::ClientSetup(format!("invalid IMAP host '{host}'")))?;

    let socket = TcpStream::connect((host, credentials.imap_port)).map_err(|e| {
        EmailServiceError::ClientSetup(format!(
            "failed to connect to IMAP server {host}:{}: {e}",
            credentials.imap_port
        ))
    })?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| EmailServiceError::ClientSetup(format!("failed to set read timeout: {e}")))?;
    socket
        .set_write_timeout(Some(timeout))
        .map_err(|e| EmailServiceError::ClientSetup(format!("failed to set write timeout: {e}")))?;

    let connection = ClientConnection::new(Arc::new(client_config), server_name).map_err(|e| {
        EmailServiceError::ClientSetup(format!("failed to initialize TLS client: {e}"))
    })?;
    let stream = StreamOwned::new(connection, socket);

    let mut session = BlockingImapSession::new(stream);
    session.expect_greeting()?;

    let username = credentials.email.trim();
    if username.is_empty() {
        return Err(EmailServiceError::Authentication(
            "email address cannot be empty".to_string(),
        ));
    }
    let password = credentials.app_password.trim();
    if password.is_empty() {
        return Err(EmailServiceError::Authentication(
            "email app password cannot be empty".to_string(),
        ));
    }

    session.login(username, password)?;
    Ok(session)
}

fn normalize_search_term(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn imap_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn parse_literal_size(line: &str) -> Option<usize> {
    let line = line.trim_end();
    let close = line.rfind('}')?;
    if close + 1 != line.len() {
        return None;
    }
    let open = line[..close].rfind('{')?;
    if open + 1 >= close {
        return None;
    }

    line[open + 1..close].parse::<usize>().ok()
}

fn capture_numeric_token(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key} ");
    let start = line.find(&marker)? + marker.len();
    let value: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if value.is_empty() { None } else { Some(value) }
}

fn capture_quoted_token(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key} ");
    let start = line.find(&marker)? + marker.len();
    let rest = line[start..].trim_start();
    let mut chars = rest.chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            value.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            _ => value.push(ch),
        }
    }

    None
}

fn capture_parenthesized_token(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key} ");
    let start = line.find(&marker)? + marker.len();
    let rest = line[start..].trim_start();
    if !rest.starts_with('(') {
        return None;
    }

    let mut depth = 0usize;
    let mut output = String::new();
    for ch in rest.chars() {
        match ch {
            '(' => {
                depth = depth.saturating_add(1);
                if depth > 1 {
                    output.push(ch);
                }
            }
            ')' => {
                if depth == 1 {
                    return Some(output);
                }
                if depth > 1 {
                    output.push(ch);
                }
                depth = depth.saturating_sub(1);
            }
            _ => {
                if depth >= 1 {
                    output.push(ch);
                }
            }
        }
    }

    None
}

fn parse_headers(literal: &[u8]) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let text = String::from_utf8_lossy(literal);
    let mut current_key: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }

        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(key) = current_key.as_deref()
                && let Some(value) = headers.get_mut(key)
            {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            continue;
        };

        let key = name.trim().to_ascii_lowercase();
        headers.insert(key.clone(), value.trim().to_string());
        current_key = Some(key);
    }

    headers
}

fn split_headers_and_body(raw: &[u8]) -> (BTreeMap<String, String>, &[u8]) {
    if let Some(index) = find_subsequence(raw, b"\r\n\r\n") {
        return (parse_headers(&raw[..index]), &raw[index + 4..]);
    }
    if let Some(index) = find_subsequence(raw, b"\n\n") {
        return (parse_headers(&raw[..index]), &raw[index + 2..]);
    }
    (parse_headers(raw), &[])
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn extract_text_and_html_bodies(
    headers: &BTreeMap<String, String>,
    body: &[u8],
) -> (Option<String>, Option<String>) {
    let mut text_body = None;
    let mut html_body = None;
    extract_bodies_from_entity(headers, body, &mut text_body, &mut html_body);
    (text_body, html_body)
}

fn extract_bodies_from_entity(
    headers: &BTreeMap<String, String>,
    body: &[u8],
    text_body: &mut Option<String>,
    html_body: &mut Option<String>,
) {
    let content_type = headers
        .get("content-type")
        .cloned()
        .unwrap_or_else(|| "text/plain".to_string());
    let content_type_lower = content_type.to_ascii_lowercase();

    if content_type_lower.starts_with("multipart/")
        && let Some(boundary) = extract_boundary_parameter(&content_type)
    {
        for part in split_multipart_parts(body, &boundary) {
            let (part_headers, part_body) = split_headers_and_body(&part);
            extract_bodies_from_entity(&part_headers, part_body, text_body, html_body);
        }
        return;
    }

    let transfer_encoding = headers
        .get("content-transfer-encoding")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let decoded = decode_transfer_encoding(body, &transfer_encoding);
    let value = String::from_utf8_lossy(&decoded).trim().to_string();
    if value.is_empty() {
        return;
    }

    if content_type_lower.starts_with("text/plain") {
        if text_body.is_none() {
            *text_body = Some(value);
        }
    } else if content_type_lower.starts_with("text/html") && html_body.is_none() {
        *html_body = Some(value);
    }
}

fn extract_boundary_parameter(content_type: &str) -> Option<String> {
    for segment in content_type.split(';').skip(1) {
        let (key, value) = segment.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("boundary") {
            let boundary = value.trim().trim_matches('"').trim_matches('\'');
            if !boundary.is_empty() {
                return Some(boundary.to_string());
            }
        }
    }
    None
}

fn split_multipart_parts(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let mut parts = Vec::new();
    let boundary_marker = format!("--{boundary}");
    let closing_marker = format!("--{boundary}--");
    let body_text = String::from_utf8_lossy(body);

    for raw_part in body_text.split(&boundary_marker).skip(1) {
        let part = raw_part.trim_start_matches('\r').trim_start_matches('\n');
        if part.starts_with("--") || part.starts_with(&closing_marker) {
            break;
        }
        let part = part.trim_end_matches('\r').trim_end_matches('\n');
        if !part.is_empty() {
            parts.push(part.as_bytes().to_vec());
        }
    }

    parts
}

fn decode_transfer_encoding(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding {
        "base64" => decode_base64(body).unwrap_or_else(|| body.to_vec()),
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let mut values = Vec::new();
    for byte in input {
        match byte {
            b'A'..=b'Z' => values.push(Some(byte - b'A')),
            b'a'..=b'z' => values.push(Some(byte - b'a' + 26)),
            b'0'..=b'9' => values.push(Some(byte - b'0' + 52)),
            b'+' => values.push(Some(62)),
            b'/' => values.push(Some(63)),
            b'=' => values.push(None),
            b' ' | b'\t' | b'\r' | b'\n' => {}
            _ => return None,
        }
    }

    if values.is_empty() {
        return Some(Vec::new());
    }
    if values.len() % 4 != 0 {
        return None;
    }

    let mut output = Vec::new();
    for chunk in values.chunks(4) {
        let a = chunk[0]?;
        let b = chunk[1]?;
        let c = chunk[2];
        let d = chunk[3];

        output.push((a << 2) | (b >> 4));

        if let Some(c) = c {
            output.push(((b & 0x0f) << 4) | (c >> 2));
            if let Some(d) = d {
                output.push(((c & 0x03) << 6) | d);
            }
        }
    }

    Some(output)
}

fn decode_quoted_printable(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut idx = 0usize;

    while idx < input.len() {
        if input[idx] == b'=' {
            if idx + 1 < input.len() && input[idx + 1] == b'\n' {
                idx += 2;
                continue;
            }
            if idx + 2 < input.len() && input[idx + 1] == b'\r' && input[idx + 2] == b'\n' {
                idx += 3;
                continue;
            }
            if idx + 2 < input.len()
                && let (Some(high), Some(low)) =
                    (hex_value(input[idx + 1]), hex_value(input[idx + 2]))
            {
                output.push((high << 4) | low);
                idx += 3;
                continue;
            }
        }

        output.push(input[idx]);
        idx += 1;
    }

    output
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_flags(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|flag| flag.trim().trim_matches(','))
        .filter(|flag| !flag.is_empty())
        .map(ToString::to_string)
        .collect()
}

pub fn normalize_sender_rule(rule: &str) -> String {
    rule.trim().to_ascii_lowercase()
}

pub fn extract_sender_address(from_header: &str) -> Option<String> {
    let from_header = from_header.trim();
    if from_header.is_empty() {
        return None;
    }

    if let Some((start, end)) = from_header
        .rfind('<')
        .zip(from_header.rfind('>'))
        .filter(|(start, end)| start < end)
    {
        let candidate = from_header[start + 1..end].trim();
        if candidate.contains('@') {
            return Some(candidate.to_ascii_lowercase());
        }
    }

    from_header.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | '<' | '>' | ',' | ';'));
        if token.contains('@') {
            Some(token.to_ascii_lowercase())
        } else {
            None
        }
    })
}

pub fn sender_matches_rule(sender_email: &str, rule: &str) -> bool {
    let sender_email = sender_email.to_ascii_lowercase();
    let rule = normalize_sender_rule(rule);
    if let Some(domain_rule) = rule.strip_prefix('@') {
        if domain_rule.is_empty() {
            return false;
        }
        sender_email.ends_with(&format!("@{domain_rule}"))
    } else {
        sender_email == rule
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_sender_address_with_display_name() {
        let sender = extract_sender_address("Alice Example <Alice@Example.com>").unwrap();
        assert_eq!(sender, "alice@example.com");
    }

    #[test]
    fn test_extract_sender_address_plain_email() {
        let sender = extract_sender_address("user@example.com").unwrap();
        assert_eq!(sender, "user@example.com");
    }

    #[test]
    fn test_sender_matches_rule_exact() {
        assert!(sender_matches_rule(
            "alice@example.com",
            "ALICE@EXAMPLE.COM"
        ));
        assert!(!sender_matches_rule("alice@example.com", "bob@example.com"));
    }

    #[test]
    fn test_sender_matches_rule_domain() {
        assert!(sender_matches_rule("alice@example.com", "@example.com"));
        assert!(!sender_matches_rule("alice@other.com", "@example.com"));
    }

    #[test]
    fn test_allowlist_policy() {
        let policy = EmailPolicy {
            mode: EmailMode::Allowlist,
            sender_rules: vec![normalize_sender_rule("@trusted.local")],
            default_max_results: 50,
        };
        assert!(policy.sender_visible("Alice <alice@trusted.local>"));
        assert!(!policy.sender_visible("Bob <bob@untrusted.local>"));
    }

    #[test]
    fn test_denylist_policy() {
        let policy = EmailPolicy {
            mode: EmailMode::Denylist,
            sender_rules: vec![normalize_sender_rule("@blocked.local")],
            default_max_results: 50,
        };
        assert!(!policy.sender_visible("Spam <offer@blocked.local>"));
        assert!(policy.sender_visible("News <news@trusted.local>"));
    }

    #[test]
    fn test_parse_literal_size() {
        assert_eq!(
            super::parse_literal_size("* 1 FETCH (BODY[] {123}"),
            Some(123)
        );
        assert_eq!(super::parse_literal_size("* OK ready"), None);
    }

    #[test]
    fn test_parse_headers() {
        let literal = b"From: Alice <alice@example.com>\r\nSubject: Hello\r\nDate: Wed, 1 Jan 2025 00:00:00 +0000\r\n\r\n";
        let headers = super::parse_headers(literal);

        assert_eq!(
            headers.get("from"),
            Some(&"Alice <alice@example.com>".to_string())
        );
        assert_eq!(headers.get("subject"), Some(&"Hello".to_string()));
        assert_eq!(
            headers.get("date"),
            Some(&"Wed, 1 Jan 2025 00:00:00 +0000".to_string())
        );
    }

    #[test]
    fn test_extract_text_and_html_bodies_multipart() {
        let raw = b"From: Alice <alice@example.com>\r\nContent-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nPlain body\r\n--b1\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body>HTML body</body></html>\r\n--b1--\r\n";
        let (headers, body) = split_headers_and_body(raw);
        let (text_body, html_body) = extract_text_and_html_bodies(&headers, body);

        assert_eq!(text_body.as_deref(), Some("Plain body"));
        assert_eq!(
            html_body.as_deref(),
            Some("<html><body>HTML body</body></html>")
        );
    }

    #[test]
    fn test_extract_text_body_quoted_printable() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHello=2C=20World=21\r\n";
        let (headers, body) = split_headers_and_body(raw);
        let (text_body, html_body) = extract_text_and_html_bodies(&headers, body);

        assert_eq!(text_body.as_deref(), Some("Hello, World!"));
        assert!(html_body.is_none());
    }

    #[test]
    fn test_extract_html_body_base64() {
        let raw = b"Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPGgxPkhlbGxvPC9oMT4=\r\n";
        let (headers, body) = split_headers_and_body(raw);
        let (text_body, html_body) = extract_text_and_html_bodies(&headers, body);

        assert!(text_body.is_none());
        assert_eq!(html_body.as_deref(), Some("<h1>Hello</h1>"));
    }
}
