//! Conversion from Responses API request format to Chat Completions API request format.

use dcode_protocol::models::ContentItem;
use dcode_protocol::models::ResponseItem;
use serde_json::Value;
use serde_json::json;

use crate::error::ApiError;

/// Default max_tokens for Chat Completions API.
/// Keeps output bounded to avoid excessive credit consumption on providers
/// like GitHub Copilot that bill based on allocated context window.
const DEFAULT_MAX_TOKENS: u64 = 16384;

/// Build a Chat Completions request JSON body from the given components.
pub fn build_chat_request(
    model: &str,
    instructions: &str,
    input: &[ResponseItem],
    tools_json: &[Value],
    parallel_tool_calls: bool,
    max_output_tokens: Option<u64>,
) -> Result<Value, ApiError> {
    let mut messages: Vec<Value> = Vec::new();

    if !instructions.trim().is_empty() {
        messages.push(json!({
            "role": "system",
            "content": instructions
        }));
    }

    for item in input {
        if let Some(msg) = response_item_to_chat_message(item) {
            messages.push(msg);
        }
    }

    let chat_tools: Vec<Value> = tools_json
        .iter()
        .filter_map(responses_tool_to_chat_tool)
        .collect();

    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if !chat_tools.is_empty() {
        body["tools"] = json!(chat_tools);
        body["tool_choice"] = json!("auto");
        if !parallel_tool_calls {
            body["parallel_tool_calls"] = json!(false);
        }
    }

    Ok(body)
}

fn response_item_to_chat_message(item: &ResponseItem) -> Option<Value> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let content_value = content_items_to_chat_content(content);
            Some(json!({
                "role": role,
                "content": content_value
            }))
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => Some(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": arguments
                }
            }]
        })),
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let content = output.to_string();
            Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content
            }))
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => Some(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": input
                }
            }]
        })),
        ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => {
            let content = output.to_string();
            Some(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": content
            }))
        }
        // Skip Reasoning, LocalShellCall (Responses-API-specific), WebSearchCall, etc.
        _ => None,
    }
}

fn content_items_to_chat_content(items: &[ContentItem]) -> Value {
    if items.is_empty() {
        return json!("");
    }
    if items.len() == 1 {
        match &items[0] {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                return json!(text);
            }
            ContentItem::InputImage { image_url } => {
                return json!([{
                    "type": "image_url",
                    "image_url": {"url": image_url}
                }]);
            }
        }
    }
    let parts: Vec<Value> = items
        .iter()
        .map(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                json!({"type": "text", "text": text})
            }
            ContentItem::InputImage { image_url } => json!({
                "type": "image_url",
                "image_url": {"url": image_url}
            }),
        })
        .collect();
    json!(parts)
}

/// Convert a Responses API tool JSON to Chat Completions tool JSON.
///
/// Responses: `{"type":"function","name":"...","description":"...","parameters":{...}}`
/// Chat:      `{"type":"function","function":{"name":"...","description":"...","parameters":{...}}}`
fn responses_tool_to_chat_tool(tool: &Value) -> Option<Value> {
    let tool_type = tool.get("type")?.as_str()?;
    if tool_type != "function" {
        return None;
    }
    let name = tool.get("name")?.clone();
    let description = tool.get("description")?.clone();
    let parameters = tool.get("parameters").cloned().unwrap_or(json!({}));
    let mut func_def = json!({
        "name": name,
        "description": description,
        "parameters": parameters,
    });
    if let Some(strict) = tool.get("strict") {
        func_def["strict"] = strict.clone();
    }
    Some(json!({
        "type": "function",
        "function": func_def
    }))
}
