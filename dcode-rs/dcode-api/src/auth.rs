use crate::provider::AuthHeaderStyle;
use dcode_client::Request;
use http::HeaderMap;
use http::HeaderValue;

/// Provides bearer and account identity information for API requests.
///
/// Implementations should be cheap and non-blocking; any asynchronous
/// refresh or I/O should be handled by higher layers before requests
/// reach this interface.
pub trait AuthProvider: Send + Sync {
    fn bearer_token(&self) -> Option<String>;
    fn account_id(&self) -> Option<String> {
        None
    }
}

pub(crate) fn add_auth_headers_to_header_map<A: AuthProvider>(
    auth: &A,
    headers: &mut HeaderMap,
    style: AuthHeaderStyle,
) {
    if let Some(token) = auth.bearer_token() {
        match style {
            AuthHeaderStyle::Bearer => {
                if let Ok(header) = HeaderValue::from_str(&format!("Bearer {token}")) {
                    let _ = headers.insert(http::header::AUTHORIZATION, header);
                }
            }
            AuthHeaderStyle::XApiKey => {
                if let Ok(header) = HeaderValue::from_str(&token) {
                    let _ = headers.insert("x-api-key", header);
                }
            }
        }
    }
    if let Some(account_id) = auth.account_id()
        && let Ok(header) = HeaderValue::from_str(&account_id)
    {
        let _ = headers.insert("ChatGPT-Account-ID", header);
    }
}

pub(crate) fn add_auth_headers<A: AuthProvider>(
    auth: &A,
    mut req: Request,
    style: AuthHeaderStyle,
) -> Request {
    add_auth_headers_to_header_map(auth, &mut req.headers, style);
    req
}
