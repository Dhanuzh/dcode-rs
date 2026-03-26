//! HTTP client for the Anthropic native Messages API (`/messages`).

use crate::auth::AuthProvider;
use crate::common::ResponseStream;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::sse::anthropic::process_anthropic_sse;
use dcode_client::HttpTransport;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::instrument;

const MESSAGES_PATH: &str = "messages";

/// HTTP client for the Anthropic native Messages API.
pub struct AnthropicClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> AnthropicClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    #[instrument(
        name = "anthropic.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "anthropic_http",
            http.method = "POST",
            api.path = "messages"
        )
    )]
    pub async fn stream_request(
        &self,
        body: Value,
        extra_headers: HeaderMap,
    ) -> Result<ResponseStream, ApiError> {
        let stream_response = self
            .session
            .stream_with(
                Method::POST,
                MESSAGES_PATH,
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                },
            )
            .await?;

        let idle_timeout = self.session.provider().stream_idle_timeout;
        let (tx_event, rx_event) = mpsc::channel(1600);

        tokio::spawn(process_anthropic_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
        ));

        Ok(ResponseStream { rx_event })
    }
}
