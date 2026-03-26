//! Conversion from internal Responses API format to Anthropic native Messages API format.

use dcode_protocol::models::ContentItem;
use dcode_protocol::models::ResponseItem;
use serde_json::Value;
use serde_json::json;

use crate::error::ApiError;

/// Default max_tokens for Anthropic Messages API (required field).
const DEFAULT_MAX_TOKENS: u64 = 8192;

/// Build an Anthropic Messages API request JSON body.
pub fn build_anthropic_request(
    model: &str,
    instructions: &str,
    input: &[ResponseItem],
    tools_json: &[Value],
) -> Result<Value, ApiError> {
    let mut messages: Vec<Value> = Vec::new();

    for item in input {
        if let Some(msg) = response_item_to_anthropic_message(item) {
            // Anthropic requires alternating user/assistant turns — merge
            // consecutive same-role messages into one.
            if let Some(last) = messages.last_mut() {
                let last_role = last["role"].as_str().unwrap_or("");
                let new_role = msg["role"].as_str().unwrap_or("");
                if last_role == new_role {
                    // Merge content arrays.
                    let extra = msg["content"].clone();
                    if let Some(arr) = last["content"].as_array_mut() {
                        match extra {
                            Value::Array(items) => arr.extend(items),
                            other => arr.push(other),
                        }
                        continue;
                    }
                }
            }
            messages.push(msg);
        }
    }

    let anthropic_tools: Vec<Value> = tools_json
        .iter()
        .filter_map(responses_tool_to_anthropic_tool)
        .collect();

    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": DEFAULT_MAX_TOKENS,
        "stream": true,
    });

    if !instructions.trim().is_empty() {
        body["system"] = json!(instructions);
    }

    if !anthropic_tools.is_empty() {
        body["tools"] = json!(anthropic_tools);
        body["tool_choice"] = json!({"type": "auto"});
    }

    Ok(body)
}

fn response_item_to_anthropic_message(item: &ResponseItem) -> Option<Value> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let content_value = content_items_to_anthropic(content);
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
        } => {
            let input: Value = serde_json::from_str(arguments).unwrap_or(json!({}));
            Some(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input
                }]
            }))
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            let content = output.to_string();
            Some(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content
                }]
            }))
        }
        ResponseItem::CustomToolCall {
            call_id, name, input, ..
        } => {
            let input_val: Value = serde_json::from_str(input).unwrap_or(json!({}));
            Some(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": input_val
                }]
            }))
        }
        ResponseItem::CustomToolCallOutput { call_id, output, .. } => {
            let content = output.to_string();
            Some(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content
                }]
            }))
        }
        _ => None,
    }
}

fn content_items_to_anthropic(items: &[ContentItem]) -> Value {
    if items.is_empty() {
        return json!([{"type": "text", "text": ""}]);
    }
    if items.len() == 1 {
        match &items[0] {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                return json!([{"type": "text", "text": text}]);
            }
            ContentItem::InputImage { image_url } => {
                // Anthropic uses {"type":"image","source":{"type":"url","url":"..."}}
                return json!([{
                    "type": "image",
                    "source": {"type": "url", "url": image_url}
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
                "type": "image",
                "source": {"type": "url", "url": image_url}
            }),
        })
        .collect();
    json!(parts)
}

/// Convert a Responses API tool definition to Anthropic native tool format.
///
/// Responses: `{"type":"function","name":"...","description":"...","parameters":{...}}`
/// Anthropic: `{"name":"...","description":"...","input_schema":{...}}`
fn responses_tool_to_anthropic_tool(tool: &Value) -> Option<Value> {
    let tool_type = tool.get("type")?.as_str()?;
    if tool_type != "function" {
        return None;
    }
    let name = tool.get("name")?.clone();
    let description = tool.get("description").cloned().unwrap_or(json!(""));
    let parameters = tool.get("parameters").cloned().unwrap_or(json!({}));
    Some(json!({
        "name": name,
        "description": description,
        "input_schema": parameters
    }))
}
