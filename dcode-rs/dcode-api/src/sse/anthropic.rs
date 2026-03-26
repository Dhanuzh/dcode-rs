//! SSE processor for the Anthropic native Messages API streaming format.

use crate::common::ResponseEvent;
use crate::error::ApiError;
use dcode_client::ByteStream;
use dcode_protocol::models::ContentItem;
use dcode_protocol::models::ResponseItem;
use dcode_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

pub async fn process_anthropic_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
) {
    let mut stream = stream.eventsource();

    let mut response_id = String::new();
    let mut usage: Option<TokenUsage> = None;
    // Track text and tool-use blocks by index.
    let mut text_blocks: HashMap<usize, String> = HashMap::new();
    let mut tool_blocks: HashMap<usize, ToolUseAccumulator> = HashMap::new();
    // Whether we've started a message item for the current text block.
    let mut message_item_started = false;
    let message_item_id = "msg_ant_0".to_string();

    let _ = tx_event.send(Ok(ResponseEvent::Created)).await;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        let _ = start.elapsed();

        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(e))) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(e.to_string())))
                    .await;
                return;
            }
            Ok(None) => {
                // Stream closed — finalize whatever we have.
                finalize_stream(
                    &text_blocks,
                    &tool_blocks,
                    &response_id,
                    &message_item_id,
                    message_item_started,
                    usage,
                    &tx_event,
                )
                .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "idle timeout waiting for Anthropic SSE".into(),
                    )))
                    .await;
                return;
            }
        };

        let data = sse.data.trim();
        trace!("Anthropic SSE: {data}");

        let event_type = sse.event.as_str();

        match event_type {
            "message_start" => {
                if let Ok(msg) = serde_json::from_str::<MessageStartEvent>(data) {
                    response_id = msg.message.id;
                    if let Some(u) = msg.message.usage {
                        usage = Some(TokenUsage {
                            input_tokens: u.input_tokens,
                            cached_input_tokens: 0,
                            output_tokens: u.output_tokens,
                            reasoning_output_tokens: 0,
                            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
                        });
                    }
                }
            }
            "content_block_start" => {
                if let Ok(evt) = serde_json::from_str::<ContentBlockStartEvent>(data) {
                    match evt.content_block.block_type.as_str() {
                        "text" => {
                            text_blocks.insert(evt.index, evt.content_block.text.unwrap_or_default());
                        }
                        "tool_use" => {
                            tool_blocks.insert(
                                evt.index,
                                ToolUseAccumulator {
                                    id: evt.content_block.id.unwrap_or_default(),
                                    name: evt.content_block.name.unwrap_or_default(),
                                    input_json: String::new(),
                                },
                            );
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                if let Ok(evt) = serde_json::from_str::<ContentBlockDeltaEvent>(data) {
                    match evt.delta.delta_type.as_str() {
                        "text_delta" => {
                            if let Some(text) = evt.delta.text {
                                if !text.is_empty() {
                                    // Emit OutputItemAdded once before first delta.
                                    if !message_item_started {
                                        message_item_started = true;
                                        let item = ResponseItem::Message {
                                            id: Some(message_item_id.clone()),
                                            role: "assistant".into(),
                                            content: vec![],
                                            end_turn: None,
                                            phase: None,
                                        };
                                        if tx_event
                                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    text_blocks
                                        .entry(evt.index)
                                        .or_default()
                                        .push_str(&text);
                                    if tx_event
                                        .send(Ok(ResponseEvent::OutputTextDelta(text)))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) = evt.delta.partial_json {
                                if let Some(acc) = tool_blocks.get_mut(&evt.index) {
                                    acc.input_json.push_str(&partial);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Ok(evt) = serde_json::from_str::<MessageDeltaEvent>(data) {
                    if let Some(u) = evt.usage {
                        let input = usage.as_ref().map_or(0, |u| u.input_tokens);
                        usage = Some(TokenUsage {
                            input_tokens: input,
                            cached_input_tokens: 0,
                            output_tokens: u.output_tokens,
                            reasoning_output_tokens: 0,
                            total_tokens: input.saturating_add(u.output_tokens),
                        });
                    }
                    if let Some(reason) = evt.delta.stop_reason {
                        if reason == "max_tokens" {
                            let _ = tx_event
                                .send(Err(ApiError::ContextWindowExceeded))
                                .await;
                            return;
                        }
                    }
                }
            }
            "message_stop" => {
                finalize_stream(
                    &text_blocks,
                    &tool_blocks,
                    &response_id,
                    &message_item_id,
                    message_item_started,
                    usage,
                    &tx_event,
                )
                .await;
                return;
            }
            "error" => {
                if let Ok(err) = serde_json::from_str::<AnthropicErrorEvent>(data) {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(err.error.message)))
                        .await;
                } else {
                    let _ = tx_event
                        .send(Err(ApiError::Stream(format!(
                            "Anthropic stream error: {data}"
                        ))))
                        .await;
                }
                return;
            }
            "ping" | "" => {
                // Heartbeat — ignore.
            }
            _ => {
                debug!("Unknown Anthropic SSE event type: {event_type}");
            }
        }
    }
}

async fn finalize_stream(
    text_blocks: &HashMap<usize, String>,
    tool_blocks: &HashMap<usize, ToolUseAccumulator>,
    response_id: &str,
    message_item_id: &str,
    message_item_started: bool,
    usage: Option<TokenUsage>,
    tx_event: &mpsc::Sender<Result<ResponseEvent, ApiError>>,
) {
    // Emit the accumulated text block as OutputItemDone.
    let combined_text: String = {
        let mut indices: Vec<usize> = text_blocks.keys().copied().collect();
        indices.sort();
        indices
            .iter()
            .filter_map(|i| text_blocks.get(i))
            .cloned()
            .collect::<Vec<_>>()
            .join("")
    };

    if message_item_started && !combined_text.is_empty() {
        let item = ResponseItem::Message {
            id: Some(message_item_id.to_string()),
            role: "assistant".into(),
            content: vec![ContentItem::OutputText {
                text: combined_text,
            }],
            end_turn: None,
            phase: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    // Emit accumulated tool calls.
    let mut tool_indices: Vec<usize> = tool_blocks.keys().copied().collect();
    tool_indices.sort();
    for idx in tool_indices {
        let acc = &tool_blocks[&idx];
        if acc.name.is_empty() {
            continue;
        }
        let item = ResponseItem::FunctionCall {
            id: None,
            name: acc.name.clone(),
            namespace: None,
            arguments: acc.input_json.clone(),
            call_id: if acc.id.is_empty() {
                format!("call_{idx}")
            } else {
                acc.id.clone()
            },
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id: response_id.to_string(),
            token_usage: usage,
        }))
        .await;
}

#[derive(Default)]
struct ToolUseAccumulator {
    id: String,
    name: String,
    input_json: String,
}

// ── Deserialization types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct MessageStartEvent {
    message: MessageStartMessage,
}

#[derive(Deserialize)]
struct MessageStartMessage {
    id: String,
    #[serde(default)]
    usage: Option<MessageUsage>,
}

#[derive(Deserialize)]
struct MessageUsage {
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

#[derive(Deserialize)]
struct ContentBlockStartEvent {
    index: usize,
    content_block: ContentBlockInfo,
}

#[derive(Deserialize)]
struct ContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    // For text blocks
    text: Option<String>,
    // For tool_use blocks
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlockDeltaEvent {
    index: usize,
    delta: ContentBlockDelta,
}

#[derive(Deserialize)]
struct ContentBlockDelta {
    #[serde(rename = "type")]
    delta_type: String,
    // For text_delta
    text: Option<String>,
    // For input_json_delta
    partial_json: Option<String>,
}

#[derive(Deserialize)]
struct MessageDeltaEvent {
    delta: MessageDelta,
    #[serde(default)]
    usage: Option<MessageDeltaUsage>,
}

#[derive(Deserialize)]
struct MessageDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: i64,
}

#[derive(Deserialize)]
struct AnthropicErrorEvent {
    error: AnthropicErrorBody,
}

#[derive(Deserialize)]
struct AnthropicErrorBody {
    message: String,
}
