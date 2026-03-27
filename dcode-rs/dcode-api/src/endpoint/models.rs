use crate::auth::AuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use dcode_client::HttpTransport;
use dcode_client::RequestTelemetry;
use dcode_protocol::openai_models::ModelInfo;
use dcode_protocol::openai_models::ModelsResponse;
use http::HeaderMap;
use http::Method;
use http::header::ETAG;
use serde::Deserialize;
use std::sync::Arc;

pub struct ModelsClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> ModelsClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "models"
    }

    fn append_client_version_query(req: &mut dcode_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub async fn list_models(
        &self,
        client_version: &str,
        extra_headers: HeaderMap,
    ) -> Result<(Vec<ModelInfo>, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(
                Method::GET,
                Self::path(),
                extra_headers,
                /*body*/ None,
                |req| {
                    Self::append_client_version_query(req, client_version);
                },
            )
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        let ModelsResponse { models } = serde_json::from_slice::<ModelsResponse>(&resp.body)
            .map_err(|e| {
                ApiError::Stream(format!(
                    "failed to decode models response: {e}; body: {}",
                    String::from_utf8_lossy(&resp.body)
                ))
            })?;

        Ok((models, header_etag))
    }
}

/// Minimal model entry as returned by the standard OpenAI-compatible `/models`
/// endpoint: `{"data": [{"id": "gpt-4o", ...}]}`.
#[derive(Debug, Deserialize)]
struct SimpleModelEntry {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SimpleModelsListResponse {
    data: Vec<SimpleModelEntry>,
}

/// A lightweight client that fetches models from any OpenAI-compatible
/// `/models` endpoint (the `{"data": [...]}` format used by GitHub Copilot,
/// Groq, Mistral, etc.) and converts the result into minimal `ModelInfo`
/// records with no upgrade/migration metadata.
pub struct SimpleModelsClient<T: HttpTransport, A: AuthProvider> {
    session: EndpointSession<T, A>,
}

impl<T: HttpTransport, A: AuthProvider> SimpleModelsClient<T, A> {
    pub fn new(transport: T, provider: Provider, auth: A) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, ApiError> {
        let resp = self
            .session
            .execute(Method::GET, "models", HeaderMap::new(), None)
            .await?;

        let list = serde_json::from_slice::<SimpleModelsListResponse>(&resp.body).map_err(|e| {
            ApiError::Stream(format!(
                "failed to decode simple models response: {e}; body: {}",
                String::from_utf8_lossy(&resp.body)
            ))
        })?;

        let models = list
            .data
            .into_iter()
            .map(|entry| ModelInfo::for_custom_provider(entry.id))
            .collect();
        Ok(models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use async_trait::async_trait;
    use dcode_client::Request;
    use dcode_client::Response;
    use dcode_client::StreamResponse;
    use dcode_client::TransportError;
    use http::HeaderMap;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct CapturingTransport {
        last_request: Arc<Mutex<Option<Request>>>,
        body: Arc<ModelsResponse>,
        etag: Option<String>,
    }

    impl Default for CapturingTransport {
        fn default() -> Self {
            Self {
                last_request: Arc::new(Mutex::new(None)),
                body: Arc::new(ModelsResponse { models: Vec::new() }),
                etag: None,
            }
        }
    }

    #[async_trait]
    impl HttpTransport for CapturingTransport {
        async fn execute(&self, req: Request) -> Result<Response, TransportError> {
            *self.last_request.lock().unwrap() = Some(req);
            let body = serde_json::to_vec(&*self.body).unwrap();
            let mut headers = HeaderMap::new();
            if let Some(etag) = &self.etag {
                headers.insert(ETAG, etag.parse().unwrap());
            }
            Ok(Response {
                status: StatusCode::OK,
                headers,
                body: body.into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    #[derive(Clone, Default)]
    struct DummyAuth;

    impl AuthProvider for DummyAuth {
        fn bearer_token(&self) -> Option<String> {
            None
        }
    }

    fn provider(base_url: &str) -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
            auth_header_style: Default::default(),
        }
    }

    #[tokio::test]
    async fn appends_client_version_query() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let client = ModelsClient::new(
            transport.clone(),
            provider("https://example.com/api/dcode"),
            DummyAuth,
        );

        let (models, _) = client
            .list_models("0.99.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);

        let url = transport
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .url
            .clone();
        assert_eq!(
            url,
            "https://example.com/api/dcode/models?client_version=0.99.0"
        );
    }

    #[tokio::test]
    async fn parses_models_response() {
        let response = ModelsResponse {
            models: vec![
                serde_json::from_value(json!({
                    "slug": "gpt-test",
                    "display_name": "gpt-test",
                    "description": "desc",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}, {"effort": "high", "description": "high"}],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "minimal_client_version": [0, 99, 0],
                    "supported_in_api": true,
                    "priority": 1,
                    "upgrade": null,
                    "base_instructions": "base instructions",
                    "supports_reasoning_summaries": false,
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "truncation_policy": {"mode": "bytes", "limit": 10_000},
                    "supports_parallel_tool_calls": false,
                    "supports_image_detail_original": false,
                    "context_window": 272_000,
                    "experimental_supported_tools": [],
                }))
                .unwrap(),
            ],
        };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let client = ModelsClient::new(
            transport,
            provider("https://example.com/api/dcode"),
            DummyAuth,
        );

        let (models, _) = client
            .list_models("0.99.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-test");
        assert_eq!(models[0].supported_in_api, true);
        assert_eq!(models[0].priority, 1);
    }

    #[tokio::test]
    async fn list_models_includes_etag() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: Some("\"abc\"".to_string()),
        };

        let client = ModelsClient::new(
            transport,
            provider("https://example.com/api/dcode"),
            DummyAuth,
        );

        let (models, etag) = client
            .list_models("0.1.0", HeaderMap::new())
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);
        assert_eq!(etag, Some("\"abc\"".to_string()));
    }
}
