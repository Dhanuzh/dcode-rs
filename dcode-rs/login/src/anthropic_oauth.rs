//! Anthropic Claude OAuth PKCE flow.
//!
//! This implements the same OAuth flow used by Claude Code and the Claude CLI:
//! - PKCE (S256) authorization code flow
//! - Client ID: `9d1c250a-e61b-44d9-88ed-5944d1962f5e`
//! - Authorize URL: `https://claude.ai/oauth/authorize`
//! - Token URL: `https://console.anthropic.com/v1/oauth/token`
//!
//! Flow:
//! 1. Call `create_authorization_url()` → get a URL + verifier
//! 2. User opens the URL in their browser, authorizes, and receives a code
//! 3. User pastes the code into the CLI
//! 4. Call `exchange_code_for_token(code, verifier)` → access token
//! 5. Store the access token (via `login_with_api_key`) for use with the Anthropic provider

use std::io;

use serde::Deserialize;

use crate::pkce::generate_pkce;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";
pub const API_USER_AGENT: &str = "claude-cli/2.1.80 (external, cli)";

/// Data returned from `create_authorization_url`.
#[derive(Debug, Clone)]
pub struct AnthropicOAuthRequest {
    /// URL the user must open in their browser.
    pub url: String,
    /// PKCE verifier — keep this secret until `exchange_code_for_token`.
    pub verifier: String,
}

/// Build an Anthropic OAuth authorization URL with PKCE.
pub fn create_authorization_url() -> AnthropicOAuthRequest {
    let pkce = generate_pkce();
    let params = [
        ("code", "true"),
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.code_verifier),
    ];

    let query: String = params
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                k,
                url_encode(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&");

    AnthropicOAuthRequest {
        url: format!("{AUTHORIZE_URL}?{query}"),
        verifier: pkce.code_verifier,
    }
}

/// Strip a `#state=…` fragment that the callback page may append to the code.
fn parse_auth_code(raw: &str) -> &str {
    match raw.find('#') {
        Some(idx) => &raw[..idx],
        None => raw,
    }
}

/// Simple percent-encoding for OAuth query parameters.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    expires_in: Option<u64>,
}

/// Exchange an authorization code (pasted by the user) for an access token.
///
/// Returns the `access_token` that should be stored via `login_with_api_key` or
/// as `experimental_bearer_token` in config.toml.
pub async fn exchange_code_for_token(raw_code: &str, verifier: &str) -> io::Result<String> {
    let code = parse_auth_code(raw_code.trim()).to_string();

    let body = format!(
        "grant_type=authorization_code&code={}&code_verifier={}&client_id={}&redirect_uri={}&state={}",
        url_encode(&code),
        url_encode(verifier),
        url_encode(CLIENT_ID),
        url_encode(REDIRECT_URI),
        url_encode(verifier),
    );

    let client = reqwest::Client::builder()
        .build()
        .map_err(io::Error::other)?;

    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", API_USER_AGENT)
        .body(body)
        .send()
        .await
        .map_err(io::Error::other)?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(io::Error::other(format!(
            "Anthropic token exchange failed: {status} — {text}"
        )));
    }

    let data: TokenResponse = resp.json().await.map_err(io::Error::other)?;
    Ok(data.access_token)
}
