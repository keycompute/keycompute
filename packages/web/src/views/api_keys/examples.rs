//! API Key 快速使用示例文本生成。
//!
//! 与后端协议网关对应：OpenAI Chat Completions、OpenAI Responses 与
//! Anthropic Messages 各有一套可直接使用的示例。
//! 示例文本生成与视图解耦，便于单元测试，防止占位符与参数失配导致
//! 复制出去的示例不可用。

#[cfg(test)]
use client_api::api::openai::ModelInfo;

/// Anthropic 示例的默认模型：列表中没有 Anthropic 兼容模型时显示的空模型。
/// 与后端 openai 接口的空模型占位一致（见 server list_models 的 model-empty），
/// 提示用户该模型不可用、需自行替换为实际可用的模型。
/// `pub`：供视图层把模型名代入翻译文案（见 list.rs 的 example_note_anthropic）。
#[cfg(test)]
pub const DEFAULT_ANTHROPIC_MODEL: &str = "model-empty";
/// Responses 目录为空时使用不可执行的显式占位，避免把 chat-only 模型
/// 展示成可直接调用的 Responses 模型。
#[cfg(test)]
pub const DEFAULT_RESPONSES_MODEL: &str = "model-empty";

/// 一套四种示例文本（env / python / node / curl）
pub struct ApiExamples {
    pub env: String,
    pub python: String,
    pub node: String,
    pub curl: String,
    pub websocket: String,
}

impl ApiExamples {
    /// 按 tab 名取对应示例；未知 tab 回退到 env
    pub fn for_tab(&self, tab: &str) -> &str {
        match tab {
            "python" => &self.python,
            "node" => &self.node,
            "curl" => &self.curl,
            "websocket" => &self.websocket,
            _ => &self.env,
        }
    }
}

/// OpenAI 兼容协议示例（调用 /v1/chat/completions）
pub fn openai_examples(
    api_url: &str,
    api_key: &str,
    model: &str,
    env_comment: &str,
) -> ApiExamples {
    ApiExamples {
        env: format!(
            r#"# {}
API_URL="{}"
API_KEY="{}"
API_MODEL="{}""#,
            env_comment, api_url, api_key, model
        ),
        python: format!(
            r#"from openai import OpenAI

client = OpenAI(
    base_url="{}",
    api_key="{}",
)

response = client.chat.completions.create(
    model="{}",
    messages=[{{"role": "user", "content": "Hello"}}],
)

print(response.choices[0].message.content)"#,
            api_url, api_key, model
        ),
        node: format!(
            r#"import OpenAI from "openai";

const client = new OpenAI({{
  baseURL: "{}",
  apiKey: "{}",
}});

const response = await client.chat.completions.create({{
  model: "{}",
  messages: [{{ role: "user", content: "Hello" }}],
}});

console.log(response.choices[0].message.content);"#,
            api_url, api_key, model
        ),
        curl: format!(
            r#"curl "{}/chat/completions" \
    -H "Authorization: Bearer {}" \
    -H "Content-Type: application/json" \
  -d '{{
    "model": "{}",
    "messages": [
      {{"role": "user", "content": "Hello"}}
    ]
  }}'"#,
            api_url, api_key, model
        ),
        websocket: String::new(),
    }
}

/// OpenAI Responses 示例。HTTP JSON、HTTP SSE 与 WebSocket transport 共用
/// 同一 `{base_url}/responses` 资源路径。
pub fn responses_examples(
    api_url: &str,
    api_key: &str,
    model: &str,
    env_comment: &str,
) -> ApiExamples {
    responses_examples_inner(api_url, api_key, model, env_comment, false)
}

pub fn stateless_responses_examples(
    api_url: &str,
    api_key: &str,
    model: &str,
    env_comment: &str,
) -> ApiExamples {
    responses_examples_inner(api_url, api_key, model, env_comment, true)
}

fn responses_examples_inner(
    api_url: &str,
    api_key: &str,
    model: &str,
    env_comment: &str,
    stateless: bool,
) -> ApiExamples {
    let python_state = if stateless { "    store=False,\n" } else { "" };
    let json_state = if stateless {
        "    \"store\": false,\n"
    } else {
        ""
    };
    let js_state = if stateless { "  store: false,\n" } else { "" };
    let api_url = api_url.trim_end_matches('/');
    let websocket_url = if let Some(rest) = api_url.strip_prefix("https://") {
        format!("wss://{rest}/responses")
    } else if let Some(rest) = api_url.strip_prefix("http://") {
        format!("ws://{rest}/responses")
    } else {
        format!("{api_url}/responses")
    };
    ApiExamples {
        env: format!(
            r#"# {}
API_URL="{}"
API_KEY="{}"
API_MODEL="{}""#,
            env_comment, api_url, api_key, model
        ),
        python: format!(
            r#"from openai import OpenAI

client = OpenAI(
    base_url="{}",
    api_key="{}",
)

response = client.responses.create(
{python_state}    model="{}",
    input="Hello",
)

print(response.output_text)"#,
            api_url, api_key, model
        ),
        node: format!(
            r#"import OpenAI from "openai";

const client = new OpenAI({{
  baseURL: "{}",
  apiKey: "{}",
}});

const response = await client.responses.create({{
{js_state}  model: "{}",
  input: "Hello",
}});

console.log(response.output_text);"#,
            api_url, api_key, model
        ),
        curl: format!(
            r#"curl "{}/responses" \
    -H "Authorization: Bearer {}" \
    -H "Content-Type: application/json" \
  -d '{{
{json_state}    "model": "{}",
    "input": "Hello"
  }}'"#,
            api_url, api_key, model
        ),
        websocket: if stateless {
            String::new()
        } else {
            format!(
                r#"from websocket import create_connection
import json

ws = create_connection(
    "{}",
    header=["Authorization: Bearer {}"],
)

# KeyCompute WebSocket mode currently supports response.create events only.
ws.send(json.dumps({{
    "type": "response.create",
    "stream_id": "main",
    "model": "{}",
    "store": False,
    "input": "Hello",
}}))

while True:
    event = json.loads(ws.recv())
    print(event)
    if event.get("type") in {{"response.completed", "response.failed", "response.incomplete", "error"}}:
        break

ws.close()"#,
                websocket_url, api_key, model
            )
        },
    }
}

/// Anthropic Messages 协议示例（调用 /v1/messages）。
///
/// `api_root` 必须是不含 `/v1` 的根路径：官方 Anthropic SDK 会在 base_url
/// 后自行追加 `/v1/messages`，传以 `/v1` 结尾的地址会拼出 `/v1/v1/messages`。
pub fn anthropic_examples(
    api_root: &str,
    api_key: &str,
    model: &str,
    env_comment: &str,
) -> ApiExamples {
    // 防御尾斜杠：调用方可能传入 "http://gw.example.com/"，
    // 与 api_client 的 normalize 惯例保持一致，避免拼出 //v1/messages。
    let root = api_root.trim_end_matches('/');
    ApiExamples {
        env: format!(
            r#"# {}
ANTHROPIC_BASE_URL="{}"
ANTHROPIC_API_KEY="{}""#,
            env_comment, root, api_key
        ),
        python: format!(
            r#"from anthropic import Anthropic

client = Anthropic(
    base_url="{}",
    api_key="{}",
)

message = client.messages.create(
    model="{}",
    max_tokens=1024,
    messages=[{{"role": "user", "content": "Hello"}}],
)

print(message.content[0].text)"#,
            root, api_key, model
        ),
        node: format!(
            r#"import Anthropic from "@anthropic-ai/sdk";

const client = new Anthropic({{
  baseURL: "{}",
  apiKey: "{}",
}});

const message = await client.messages.create({{
  model: "{}",
  max_tokens: 1024,
  messages: [{{ role: "user", content: "Hello" }}],
}});

console.log(message.content[0].text);"#,
            root, api_key, model
        ),
        curl: format!(
            r#"curl "{}/v1/messages" \
    -H "x-api-key: {}" \
    -H "anthropic-version: 2023-06-01" \
    -H "Content-Type: application/json" \
  -d '{{
    "model": "{}",
    "max_tokens": 1024,
    "messages": [
      {{"role": "user", "content": "Hello"}}
    ]
  }}'"#,
            root, api_key, model
        ),
        websocket: String::new(),
    }
}

/// 从模型列表中选取展示用的默认模型（列表为空时回退 deepseek-chat）
#[cfg(test)]
pub fn pick_sample_model(models: &[ModelInfo]) -> String {
    models
        .first()
        .map(|model| model.id.clone())
        .unwrap_or_else(|| "deepseek-chat".to_string())
}

/// 从 Responses-capable 模型目录选择示例模型；空目录不回退到 chat-only
/// 默认模型，保留显式的不可用占位。
#[cfg(test)]
pub fn pick_responses_model(models: &[ModelInfo]) -> String {
    models
        .first()
        .map(|model| model.id.clone())
        .unwrap_or_else(|| DEFAULT_RESPONSES_MODEL.to_string())
}

/// 从模型列表中选取 Anthropic 示例模型：优先第一个 Claude 模型
/// （大小写不敏感）；列表中没有 Claude 模型时取列表中第一个
/// Anthropic 兼容模型；列表为空时显示空模型
/// （DEFAULT_ANTHROPIC_MODEL，与 openai 空模型一致），提示用户不可用。
#[cfg(test)]
pub fn pick_anthropic_model(models: &[ModelInfo]) -> String {
    models
        .iter()
        .map(|model| model.id.clone())
        .find(|id| id.to_lowercase().starts_with("claude"))
        .or_else(|| models.first().map(|model| model.id.clone()))
        .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言一段示例文本不含未替换的 `{}` 占位符（format 参数与占位符失配的哨兵）
    fn assert_no_leftover_placeholder(text: &str) {
        assert!(!text.contains("{}"), "示例文本残留未替换占位符: {text}");
    }

    fn model(id: &str) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            object: "model".to_string(),
            created: 0,
            owned_by: "test".to_string(),
        }
    }

    #[test]
    fn openai_examples_are_fully_formatted() {
        let e = openai_examples(
            "http://gw.example.com/v1",
            "sk-test",
            "deepseek-chat",
            "# env",
        );
        for text in [&e.env, &e.python, &e.node, &e.curl, &e.websocket] {
            assert_no_leftover_placeholder(text);
        }
        assert!(e.python.contains("from openai import OpenAI"));
        assert!(
            e.curl
                .contains("\"http://gw.example.com/v1/chat/completions\"")
        );
        assert!(e.curl.contains("-H \"Authorization: Bearer sk-test\""));
    }

    #[test]
    fn responses_examples_cover_http_and_websocket_without_renaming_env_vars() {
        let e = responses_examples("https://gw.example.com/v1", "sk-test", "gpt-5", "# env");
        for text in [&e.env, &e.python, &e.node, &e.curl, &e.websocket] {
            assert_no_leftover_placeholder(text);
        }
        assert!(e.env.contains("API_URL=\"https://gw.example.com/v1\""));
        assert!(e.env.contains("API_KEY=\"sk-test\""));
        assert!(e.env.contains("API_MODEL=\"gpt-5\""));
        assert!(e.python.contains("client.responses.create("));
        assert!(e.node.contains("client.responses.create({"));
        assert!(e.curl.contains("https://gw.example.com/v1/responses"));
        assert!(e.websocket.contains("wss://gw.example.com/v1/responses"));
        assert!(e.websocket.contains("supports response.create events only"));
        assert!(e.websocket.contains("\"type\": \"response.create\""));
    }

    #[test]
    fn anthropic_examples_are_fully_formatted() {
        let e = anthropic_examples(
            "http://gw.example.com",
            "sk-test",
            "claude-3-5-sonnet-20241022",
            "# env",
        );
        for text in [&e.env, &e.python, &e.node, &e.curl] {
            assert_no_leftover_placeholder(text);
        }
        assert!(e.python.contains("from anthropic import Anthropic"));
        assert!(e.python.contains("max_tokens=1024"));
    }

    /// Anthropic SDK 会在 base_url 后自行追加 /v1/messages，示例必须使用
    /// 不含 /v1 的根路径，否则会拼出 /v1/v1/messages
    #[test]
    fn anthropic_sdk_examples_use_root_url_without_v1() {
        let e = anthropic_examples(
            "http://gw.example.com",
            "sk-test",
            "claude-3-5-sonnet",
            "# env",
        );
        assert!(e.python.contains("base_url=\"http://gw.example.com\""));
        assert!(e.node.contains("baseURL: \"http://gw.example.com\""));
        assert!(
            e.env
                .contains("ANTHROPIC_BASE_URL=\"http://gw.example.com\"")
        );
        assert!(!e.python.contains("v1/v1"));
        assert!(!e.node.contains("v1/v1"));
    }

    /// curl 直接指向网关挂载点 {root}/v1/messages
    #[test]
    fn anthropic_curl_targets_messages_endpoint() {
        let e = anthropic_examples(
            "http://gw.example.com",
            "sk-test",
            "claude-3-5-sonnet",
            "# env",
        );
        assert!(e.curl.contains("\"http://gw.example.com/v1/messages\""));
        assert!(e.curl.contains("-H \"x-api-key: sk-test\""));
        assert!(e.curl.contains("-H \"anthropic-version: 2023-06-01\""));
    }

    /// 尾斜杠防御：api_root 带尾斜杠时不得拼出 //v1/messages
    #[test]
    fn anthropic_examples_tolerate_trailing_slash_on_root() {
        let e = anthropic_examples(
            "http://gw.example.com/",
            "sk-test",
            "claude-3-5-sonnet",
            "# env",
        );
        assert!(e.curl.contains("\"http://gw.example.com/v1/messages\""));
        assert!(!e.curl.contains("//v1"));
        assert!(e.python.contains("base_url=\"http://gw.example.com\""));
    }

    #[test]
    fn for_tab_maps_all_tabs_and_falls_back_to_env() {
        let e = anthropic_examples("http://gw.example.com", "sk-test", "claude", "# env");
        assert_eq!(e.for_tab("python"), &e.python);
        assert_eq!(e.for_tab("node"), &e.node);
        assert_eq!(e.for_tab("curl"), &e.curl);
        assert_eq!(e.for_tab("websocket"), &e.websocket);
        assert_eq!(e.for_tab("env"), &e.env);
        assert_eq!(e.for_tab("unknown"), &e.env);
    }

    #[test]
    fn pick_anthropic_model_prefers_first_claude_case_insensitive() {
        let models = [
            model("deepseek-chat"),
            model("Claude-3-5-Sonnet"),
            model("claude-3-opus"),
        ];
        assert_eq!(pick_anthropic_model(&models), "Claude-3-5-Sonnet");
    }

    #[test]
    fn pick_anthropic_model_falls_back_to_first_model_when_no_claude() {
        // 列表非空但无 Claude 模型时，示例取列表中第一个 Anthropic 兼容模型
        assert_eq!(
            pick_anthropic_model(&[model("deepseek-chat"), model("gpt-4o")]),
            "deepseek-chat"
        );
        assert_eq!(
            pick_anthropic_model(&[model("deepseek-v4-flash")]),
            "deepseek-v4-flash"
        );
    }

    #[test]
    fn pick_anthropic_model_uses_empty_model_when_list_empty() {
        assert_eq!(pick_anthropic_model(&[]), DEFAULT_ANTHROPIC_MODEL);
    }

    #[test]
    fn pick_sample_model_returns_first_or_fallback() {
        assert_eq!(
            pick_sample_model(&[model("gpt-4o"), model("gpt-4o-mini")]),
            "gpt-4o"
        );
        assert_eq!(pick_sample_model(&[]), "deepseek-chat");
    }

    #[test]
    fn pick_responses_model_never_falls_back_to_a_chat_only_model() {
        assert_eq!(pick_responses_model(&[model("gpt-5")]), "gpt-5");
        assert_eq!(pick_responses_model(&[]), DEFAULT_RESPONSES_MODEL);
    }
    #[test]
    fn scoped_stateless_responses_examples_do_not_promise_state_or_websockets() {
        let examples = stateless_responses_examples(
            "https://example.test/nt/v1",
            "test-key",
            "raw-model",
            "test",
        );
        assert!(examples.curl.contains("/nt/v1/responses"));
        assert!(examples.curl.contains("\"store\": false"));
        assert!(examples.python.contains("store=False"));
        assert!(examples.node.contains("store: false"));
        assert!(examples.websocket.is_empty());
    }
}

/// Native SSE examples print complete protocol events, including tool/thinking
/// deltas, rather than projecting every protocol into plain text.
pub fn native_stream_examples(
    base: &str,
    root: &str,
    key: &str,
    model: &str,
    surface: &str,
    env_comment: &str,
    stateless: bool,
) -> ApiExamples {
    let messages = surface == "messages";
    let responses = surface == "responses";
    let sdk_base = if messages { root } else { base };
    let path = if messages {
        format!("{}/v1/messages", root.trim_end_matches('/'))
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            if responses {
                "responses"
            } else {
                "chat/completions"
            }
        )
    };
    let mut body = serde_json::json!({"model":model,"stream":true});
    if responses {
        body["input"] = serde_json::json!("Hello");
    } else {
        body["messages"] = serde_json::json!([{"role":"user","content":"Hello"}]);
    }
    if messages {
        body["max_tokens"] = 1024.into();
    }
    if responses && stateless {
        body["store"] = false.into();
    }
    let quoted_model = serde_json::to_string(model).expect("string serialization");
    let quoted_base = serde_json::to_string(sdk_base).expect("string serialization");
    let quoted_key = serde_json::to_string(key).expect("string serialization");
    let data = serde_json::to_string_pretty(&body).expect("example serialization");
    let shell = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let auth = if messages {
        format!("x-api-key: {key}")
    } else {
        format!("Authorization: Bearer {key}")
    };
    let version = if messages {
        " \\\n  -H 'anthropic-version: 2023-06-01'"
    } else {
        ""
    };
    let curl = format!(
        "curl --fail-with-body -N {} \\\n  -H {} \\\n  -H 'Content-Type: application/json'{} \\\n  -d {}",
        shell(&path),
        shell(&auth),
        version,
        shell(&data)
    );
    let package = if messages { "anthropic" } else { "openai" };
    let class = if messages { "Anthropic" } else { "OpenAI" };
    let method = if messages {
        "messages.create"
    } else if responses {
        "responses.create"
    } else {
        "chat.completions.create"
    };
    let py_input = if responses {
        "    input=\"Hello\",\n"
    } else {
        "    messages=[{\"role\": \"user\", \"content\": \"Hello\"}],\n"
    };
    let py_extra = if messages {
        "    max_tokens=1024,\n"
    } else if responses && stateless {
        "    store=False,\n"
    } else {
        ""
    };
    let python = format!(
        "from {package} import {class}\n\nclient = {class}(base_url={quoted_base}, api_key={quoted_key})\nstream = client.{method}(\n    model={quoted_model},\n{py_input}{py_extra}    stream=True,\n)\nfor event in stream:\n    print(event.model_dump_json())"
    );
    let js_package = if messages {
        "@anthropic-ai/sdk"
    } else {
        "openai"
    };
    let node = format!(
        "import {class} from \"{js_package}\";\n\nconst client = new {class}({{baseURL: {quoted_base}, apiKey: {quoted_key}}});\nconst stream = await client.{method}({data});\nfor await (const event of stream) {{\n  console.log(JSON.stringify(event));\n}}"
    );
    ApiExamples {
        env: format!(
            "# {env_comment}\nAPI_URL={quoted_base}\nAPI_KEY={quoted_key}\nAPI_MODEL={quoted_model}"
        ),
        python,
        node,
        curl,
        websocket: String::new(),
    }
}

#[cfg(test)]
mod native_stream_example_tests {
    use super::*;
    #[test]
    fn native_examples_preserve_url_model_protocol_events_and_stateless_controls() {
        for (surface, path) in [
            ("chat_completions", "chat/completions"),
            ("messages", "messages"),
            ("responses", "responses"),
        ] {
            let e = native_stream_examples(
                "https://example.test/prefix/nt/v1",
                "https://example.test/prefix/nt",
                "test-key",
                "org/gemma:tag",
                surface,
                "example",
                true,
            );
            assert!(e.curl.contains(&format!("/nt/v1/{path}")));
            assert!(e.curl.contains("\"stream\": true") && e.curl.contains("org/gemma:tag"));
            assert!(
                e.python.contains("model_dump_json") && e.node.contains("JSON.stringify(event)")
            );
            assert!(e.websocket.is_empty());
            if surface == "responses" {
                assert!(e.curl.contains("\"store\": false") && e.python.contains("store=False"));
            }
            if surface == "messages" {
                assert!(e.curl.contains("anthropic-version") && e.curl.contains("x-api-key"));
            }
        }
    }
}

/// Platform-managed Responses examples keep state on the selected /pt or /nt
/// base URL. They never ask the local inference runtime to store resources.
pub fn managed_response_examples(
    base: &str,
    key: &str,
    model: &str,
    workflow: &str,
    streaming: bool,
    env_comment: &str,
) -> ApiExamples {
    let base = base.trim_end_matches('/');
    let quoted = |value: &str| serde_json::to_string(value).expect("string serialization");
    let shell = |value: &str| format!("'{}'", value.replace('\'', "'\"'\"'"));
    let base_json = quoted(base);
    let key_json = quoted(key);
    let model_json = quoted(model);
    let background = workflow == "background";
    let conversation = workflow == "conversation";
    let mut body =
        serde_json::json!({"model":model,"input":"Hello","store":true,"stream":streaming});
    if background {
        body["background"] = true.into();
    }
    let body_json = serde_json::to_string_pretty(&body).expect("example serialization");
    let setup_python = if conversation {
        "conversation = client.conversations.create()\n"
    } else {
        ""
    };
    let conversation_python = if conversation {
        "    conversation=conversation.id,\n"
    } else {
        ""
    };
    let background_python = if background {
        "    background=True,\n"
    } else {
        ""
    };
    let create = format!(
        "client.responses.create(\n    model={model_json}, input=\"Hello\", store=True,\n{conversation_python}{background_python}    stream={},\n)",
        if streaming { "True" } else { "False" }
    );
    let mut python = format!(
        "import time\nfrom openai import OpenAI\n\nclient = OpenAI(base_url={base_json}, api_key={key_json}, max_retries=0)\n{setup_python}"
    );
    if streaming {
        python.push_str(&format!("stream = {create}\nresponse_id = None\nlast_sequence = None\nfor event in stream:\n    print(event.model_dump_json())\n    last_sequence = getattr(event, \"sequence_number\", last_sequence)\n    if hasattr(event, \"response\"):\n        response_id = event.response.id\nassert response_id is not None\nresponse = client.responses.retrieve(response_id)\n"));
    } else {
        python.push_str(&format!("response = {create}\n"));
    }
    if background {
        python.push_str("deadline = time.monotonic() + 120\nwhile response.status in (\"queued\", \"in_progress\"):\n    if time.monotonic() >= deadline:\n        client.responses.cancel(response.id)\n        raise TimeoutError(\"Cancelled after the polling budget\")\n    time.sleep(1)\n    response = client.responses.retrieve(response.id)\nprint(response.model_dump_json())\n");
        if streaming {
            python.push_str("# After a disconnect, resume without new inference:\n# client.responses.retrieve(response_id, stream=True, starting_after=last_sequence)\n");
        }
    } else {
        python.push_str("print(response.model_dump_json())\n");
        let ref_param = if conversation {
            "conversation=conversation.id"
        } else {
            "previous_response_id=response.id"
        };
        python.push_str(&format!("follow_up = client.responses.create(model={model_json}, input=\"Continue\", instructions=\"Answer briefly\", {ref_param}, store=True)\nprint(follow_up.model_dump_json())\n"));
    }
    let mut node_body = body.clone();
    if conversation {
        node_body["conversation"] = "CONVERSATION_ID".into();
    }
    let mut js_data = serde_json::to_string_pretty(&node_body).unwrap();
    if conversation {
        js_data = js_data.replace("\"CONVERSATION_ID\"", "conversation.id");
    }
    let setup_js = if conversation {
        "const conversation = await client.conversations.create();\n"
    } else {
        ""
    };
    let mut node = format!(
        "import OpenAI from \"openai\";\nconst client = new OpenAI({{baseURL:{base_json}, apiKey:{key_json}, maxRetries:0}});\n{setup_js}"
    );
    if streaming {
        node.push_str(&format!("const stream = await client.responses.create({js_data});\nlet responseId;\nlet lastSequence;\nfor await (const event of stream) {{\n  console.log(JSON.stringify(event));\n  lastSequence = event.sequence_number ?? lastSequence;\n  if (event.response) responseId = event.response.id;\n}}\nif (!responseId) throw new Error(\"No response ID\");\nlet response = await client.responses.retrieve(responseId);\n"));
    } else {
        node.push_str(&format!(
            "let response = await client.responses.create({js_data});\n"
        ));
    }
    if background {
        node.push_str("const deadline = Date.now() + 120000;\nwhile ([\"queued\", \"in_progress\"].includes(response.status)) {\n  if (Date.now() >= deadline) {\n    await client.responses.cancel(response.id);\n    throw new Error(\"Cancelled after polling budget\");\n  }\n  await new Promise(resolve => setTimeout(resolve, 1000));\n  response = await client.responses.retrieve(response.id);\n}\nconsole.log(JSON.stringify(response));\n");
    } else {
        let reference = if conversation {
            "conversation: conversation.id"
        } else {
            "previous_response_id: response.id"
        };
        node.push_str(&format!("console.log(JSON.stringify(response));\nconst next = await client.responses.create({{model:{model_json}, input:\"Continue\", instructions:\"Answer briefly\", {reference}, store:true}});\nconsole.log(JSON.stringify(next));\n"));
    }
    let mut curl = format!(
        "BASE_URL={}\nPLATFORM_KEY={}\nMODEL={}\n",
        shell(base),
        shell(key),
        shell(model)
    );
    if conversation {
        curl.push_str("# jq is used to insert returned identifiers safely.\nCONVERSATION_ID=$(curl --fail-with-body -sS \"$BASE_URL/conversations\" -H \"Authorization: Bearer $PLATFORM_KEY\" -H 'Content-Type: application/json' -d '{}' | jq -er .id)\n");
        curl.push_str(&format!("BODY=$(printf '%s' {} | jq --arg id \"$CONVERSATION_ID\" '. + {{conversation:$id}}')\n",shell(&body_json)));
    } else {
        curl.push_str(&format!("BODY={}\n", shell(&body_json)));
    }
    if streaming {
        curl.push_str("curl --fail-with-body -N \"$BASE_URL/responses\" -H \"Authorization: Bearer $PLATFORM_KEY\" -H 'Content-Type: application/json' -d \"$BODY\"\n# Keep response.id and sequence_number from the events above.\n");
        if background {
            curl.push_str("# Resume after a disconnect with the last received sequence number:\n# curl --fail-with-body -N \"$BASE_URL/responses/$RESPONSE_ID?stream=true&starting_after=$LAST_SEQUENCE\" -H \"Authorization: Bearer $PLATFORM_KEY\"\n");
        }
    } else {
        curl.push_str("RESPONSE=$(curl --fail-with-body -sS \"$BASE_URL/responses\" -H \"Authorization: Bearer $PLATFORM_KEY\" -H 'Content-Type: application/json' -d \"$BODY\")\nprintf '%s\\n' \"$RESPONSE\"\nRESPONSE_ID=$(printf '%s' \"$RESPONSE\" | jq -er .id)\ncurl --fail-with-body \"$BASE_URL/responses/$RESPONSE_ID\" -H \"Authorization: Bearer $PLATFORM_KEY\"\n");
        if background {
            curl.push_str("# Poll until completed/incomplete/failed/cancelled. To cancel:\n# curl --fail-with-body -X POST \"$BASE_URL/responses/$RESPONSE_ID/cancel\" -H \"Authorization: Bearer $PLATFORM_KEY\"\n");
        } else {
            let reference = if conversation {
                "--arg id \"$CONVERSATION_ID\" '{model:$model,input:\"Continue\",conversation:$id,store:true}'"
            } else {
                "--arg id \"$RESPONSE_ID\" '{model:$model,input:\"Continue\",previous_response_id:$id,store:true}'"
            };
            curl.push_str(&format!("NEXT=$(jq -n --arg model \"$MODEL\" {reference})\ncurl --fail-with-body \"$BASE_URL/responses\" -H \"Authorization: Bearer $PLATFORM_KEY\" -H 'Content-Type: application/json' -d \"$NEXT\"\n"));
        }
    }
    ApiExamples {
        env: format!(
            "# {env_comment}\nAPI_URL={base_json}\nAPI_KEY={key_json}\nAPI_MODEL={model_json}"
        ),
        python,
        node,
        curl,
        websocket: String::new(),
    }
}

#[cfg(test)]
mod managed_response_example_tests {
    use super::*;
    #[test]
    fn examples_keep_resources_in_the_same_mode_and_never_repeat_inference_to_poll() {
        for base in [
            "https://example.test/prefix/pt/v1",
            "https://example.test/prefix/nt/v1",
        ] {
            for workflow in ["stored", "conversation", "background"] {
                for stream in [false, true] {
                    let e = managed_response_examples(
                        base,
                        "key-placeholder",
                        "org/model:tag",
                        workflow,
                        stream,
                        "example",
                    );
                    assert!(
                        e.python.contains(base) && e.node.contains(base) && e.curl.contains(base)
                    );
                    assert!(e.python.contains("store=True") && e.node.contains("\"store\": true"));
                    assert!(e.python.contains("max_retries=0") && e.node.contains("maxRetries:0"));
                    assert!(e.python.contains("org/model:tag"));
                    if workflow == "background" {
                        assert!(
                            e.python.contains("time.monotonic()")
                                && e.python.contains("responses.cancel")
                        );
                    }
                    if workflow == "conversation" {
                        assert!(
                            e.python.contains("conversations.create()")
                                && !e.python.contains("previous_response_id=")
                        );
                    }
                    if workflow == "stored" {
                        assert!(e.python.contains("previous_response_id=response.id"));
                    }
                    if stream {
                        assert!(e.python.contains("model_dump_json") && e.curl.contains(" -N "));
                    }
                    assert!(e.websocket.is_empty());
                }
            }
        }
    }
}
