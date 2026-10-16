use futures::stream::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zarn_core::provider::{Event, Provider, ProviderError};
use zarn_core::types::{Message, Request, StopReason};
use zarn_providers::{AnthropicProvider, GoogleProvider, OllamaProvider, OpenAiCompatProvider};

fn test_request() -> Request {
    Request::new("test-model", vec![Message::user("test message")])
}

mod openai {
    use super::*;

    #[tokio::test]
    async fn posts_to_chat_completions_with_bearer_auth() {
        let server = MockServer::start().await;
        let _mock_request = Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta("hi".into()));

        let usage_event = stream.next().await.unwrap();
        assert_eq!(usage_event, Event::Usage { input: 10, output: 2, cached: 0 });

        let done_event = stream.next().await.unwrap();
        assert_eq!(done_event, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn parses_tool_call_events() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"file.txt\\\"}}\"}}]}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":8}}\n\ndata: [DONE]\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let mut stream = provider.stream(test_request()).await.unwrap();

        let mut events = Vec::new();
        while let Some(evt) = stream.next().await {
            events.push(evt);
        }

        let has_tool_start = events.iter().any(|e| matches!(e, Event::ToolCallStart { .. }));
        let has_tool_delta = events.iter().any(|e| matches!(e, Event::ToolCallDelta(_)));
        let has_tool_end = events.iter().any(|e| matches!(e, Event::ToolCallEnd));

        assert!(has_tool_start, "should have ToolCallStart");
        assert!(has_tool_delta, "should have ToolCallDelta");
        assert!(has_tool_end, "should have ToolCallEnd");
    }

    #[tokio::test]
    async fn preserves_multibyte_utf8_split_across_chunks() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"choices\":[{\"delta\":{\"content\":\"😀\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta("😀".into()));

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn classifies_rate_limit_as_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::RateLimit(_))));
    }

    #[tokio::test]
    async fn classifies_auth_as_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[tokio::test]
    async fn classifies_bad_request_as_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::BadRequest(_))));
    }

    #[tokio::test]
    async fn classifies_server_error_as_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&server)
            .await;

        let provider = OpenAiCompatProvider::new(server.uri(), "test-key");
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Network(_))));
    }
}

mod anthropic {
    use super::*;

    #[tokio::test]
    async fn posts_to_messages_with_api_key_and_version_headers() {
        let server = MockServer::start().await;
        let _mock = Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta("hello".into()));

        let done = stream.next().await.unwrap();
        assert!(matches!(done, Event::Done(_)));
    }


    #[tokio::test]
    async fn reassembles_tool_call_arguments_split_across_frames() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"event: content_block_start
data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"toolu_1","name":"read"}}

event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"pa"}}

event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"th\":\"a.rs\"}"}}

event: content_block_stop
data: {"type":"content_block_stop"}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}

event: message_stop
data: {"type":"message_stop"}

"#,
            ).insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let start = stream.next().await.unwrap();
        assert!(matches!(start, Event::ToolCallStart { .. }));

        let mut args = String::new();
        loop {
            let evt = stream.next().await.unwrap();
            match evt {
                Event::ToolCallDelta(s) => args.push_str(&s),
                Event::ToolCallEnd => break,
                _ => continue,
            }
        }
        assert_eq!(args, r#"{"path":"a.rs"}"#);

        let _done = stream.next().await.unwrap();
    }

    #[tokio::test]
    async fn preserves_multibyte_utf8_split_across_frames() {
        let server = MockServer::start().await;
        let emoji = "😀";
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"event: content_block_delta
data: {{"type":"content_block_delta","delta":{{"type":"text_delta","text":"{}"}}}}

event: message_stop
data: {{"type":"message_stop"}}

"#, emoji))
                .insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta(emoji.into()));

        let done = stream.next().await.unwrap();
        assert!(matches!(done, Event::Done(_)));
    }

    #[tokio::test]
    async fn classifies_rate_limit_as_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::RateLimit(_))));
    }

    #[tokio::test]
    async fn classifies_auth_as_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[tokio::test]
    async fn classifies_bad_request_as_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::BadRequest(_))));
    }

    #[tokio::test]
    async fn classifies_server_error_as_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Network(_))));
    }
}

mod google {
    use super::*;

    #[tokio::test]
    async fn requests_alt_sse_with_api_key_in_header_not_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(|req: &wiremock::Request| {
                let has_endpoint = req.url.path().contains("test-model:streamGenerateContent");
                let has_alt_sse = req.url.query().is_some_and(|q| q.contains("alt=sse"));
                has_endpoint && has_alt_sse
            })
            .and(header("x-goog-api-key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]}}]}\n\ndata: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}]}\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta("hello".into()));

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn does_not_leak_api_key_in_error_messages() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("SECRET_API_KEY_12345").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        if let Err(ProviderError::Auth(msg)) = result {
            assert!(!msg.contains("SECRET_API_KEY_12345"));
        } else {
            panic!("Expected Auth error");
        }
    }

    #[tokio::test]
    async fn parses_sse_response_into_events() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\ndata: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":2}}\n\n",
            ).insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text = stream.next().await.unwrap();
        assert_eq!(text, Event::TextDelta("hi".into()));

        let usage = stream.next().await.unwrap();
        assert_eq!(usage, Event::Usage {
            input: 10,
            output: 2,
            cached: 0,
        });

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn preserves_multibyte_utf8_in_text_deltas() {
        let server = MockServer::start().await;
        let emoji = "🎉";
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"data: {{"candidates":[{{"content":{{"parts":[{{"text":"{}"}}]}}}}]}}

"#, emoji))
                .insert_header("content-type", "text/event-stream"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text_event = stream.next().await.unwrap();
        assert_eq!(text_event, Event::TextDelta(emoji.into()));
    }

    #[tokio::test]
    async fn classifies_rate_limit_as_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::RateLimit(_))));
    }

    #[tokio::test]
    async fn classifies_auth_as_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[tokio::test]
    async fn classifies_bad_request_as_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::BadRequest(_))));
    }

    #[tokio::test]
    async fn classifies_server_error_as_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&server)
            .await;

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Network(_))));
    }
}

mod ollama {
    use super::*;

    #[tokio::test]
    async fn posts_to_api_chat_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"message\":{\"content\":\"hello\"},\"done\":false}\n{\"message\":{\"content\":\"\"},\"done\":true,\"eval_count\":5}\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text = stream.next().await.unwrap();
        assert_eq!(text, Event::TextDelta("hello".into()));

        let usage = stream.next().await.unwrap();
        assert_eq!(usage, Event::Usage {
            input: 0,
            output: 5,
            cached: 0,
        });

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }


    #[tokio::test]
    async fn reads_ndjson_stream_and_yields_text_deltas_then_usage() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"message\":{\"content\":\"h\"},\"done\":false}\n{\"message\":{\"content\":\"i\"},\"done\":false}\n{\"message\":{\"content\":\"\"},\"done\":true,\"prompt_eval_count\":10,\"eval_count\":2}\n",
            ))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let e1 = stream.next().await.unwrap();
        assert_eq!(e1, Event::TextDelta("h".into()));

        let e2 = stream.next().await.unwrap();
        assert_eq!(e2, Event::TextDelta("i".into()));

        let e3 = stream.next().await.unwrap();
        assert_eq!(e3, Event::Usage {
            input: 10,
            output: 2,
            cached: 0,
        });

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn preserves_multibyte_utf8_split_across_ndjson_lines() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"message\":{\"content\":\"😀\"},\"done\":false}\n{\"done\":true}\n",
            ))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let mut stream = provider.stream(test_request()).await.unwrap();

        let text = stream.next().await.unwrap();
        assert_eq!(text, Event::TextDelta("😀".into()));

        let done = stream.next().await.unwrap();
        assert_eq!(done, Event::Done(StopReason::EndTurn));
    }

    #[tokio::test]
    async fn classifies_rate_limit_as_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate limited"))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::RateLimit(_))));
    }

    #[tokio::test]
    async fn classifies_auth_as_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Auth(_))));
    }

    #[tokio::test]
    async fn classifies_bad_request_as_400() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::BadRequest(_))));
    }

    #[tokio::test]
    async fn classifies_server_error_as_network() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(server.uri());
        let result = provider.stream(test_request()).await;

        assert!(matches!(result, Err(ProviderError::Network(_))));
    }
}

mod request_bodies {
    use super::*;
    use serde_json::Value;
    use zarn_core::types::ToolSpec;

    async fn captured_body(server: &MockServer) -> Value {
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        serde_json::from_slice(&requests[0].body).unwrap()
    }

    fn tool(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: format!("the {name} tool"),
            schema: serde_json::json!({ "type": "object" }),
        }
    }

    fn request_with_tools(count: usize) -> Request {
        let mut request = test_request();
        request.tools = (0..count).map(|i| tool(&format!("tool{i}"))).collect();
        request
    }

    async fn empty_sse(server: &MockServer, route: &str) {
        Mock::given(method("POST"))
            .and(path(route))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("data: [DONE]\n\n")
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn anthropic_marks_only_the_last_tool_for_caching() {
        let server = MockServer::start().await;
        empty_sse(&server, "/messages").await;

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let _ = provider.stream(request_with_tools(6)).await.unwrap();

        let body = captured_body(&server).await;
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 6);

        let marked = tools
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .count();
        assert_eq!(
            marked, 1,
            "anthropic allows at most four cache breakpoints per request; marking every tool makes any request with a handful of tools fail as a 400, which the fallback chain will not retry"
        );
        assert!(tools.last().unwrap().get("cache_control").is_some());
    }

    #[tokio::test]
    async fn anthropic_keeps_total_cache_breakpoints_within_the_limit() {
        let server = MockServer::start().await;
        empty_sse(&server, "/messages").await;

        let mut request = request_with_tools(6);
        request
            .messages
            .insert(0, Message::System("you are zarn".into()));

        let provider = AnthropicProvider::new("test-key").with_base_url(server.uri());
        let _ = provider.stream(request).await.unwrap();

        let body = captured_body(&server).await;
        let count = |value: Option<&Value>| {
            value
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| i.get("cache_control").is_some())
                        .count()
                })
                .unwrap_or(0)
        };

        let total = count(body.get("tools")) + count(body.get("system"));
        assert!(total <= 4, "{total} breakpoints exceeds anthropic's limit of 4");
    }

    #[tokio::test]
    async fn ollama_nests_generation_options_where_the_server_reads_them() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"message\":{\"content\":\"\"},\"done\":true}\n",
            ))
            .mount(&server)
            .await;

        let mut request = test_request();
        request.max_tokens = 321;
        request.temperature = 0.25;

        let provider = OllamaProvider::new(server.uri());
        let _ = provider.stream(request).await.unwrap();

        let body = captured_body(&server).await;
        assert_eq!(
            body["options"]["num_predict"], 321,
            "ollama reads generation parameters from options; top-level keys are ignored and every request silently runs on defaults"
        );
        assert!((body["options"]["temperature"].as_f64().unwrap() - 0.25).abs() < 1e-6);
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_tokens").is_none());
    }

    #[tokio::test]
    async fn google_sends_tool_declarations_and_a_system_instruction() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("data: {\"candidates\":[]}\n\n")
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let mut request = request_with_tools(2);
        request
            .messages
            .insert(0, Message::System("be brief".into()));

        let provider = GoogleProvider::new("test-key").with_base_url(server.uri());
        let _ = provider.stream(request).await.unwrap();

        let body = captured_body(&server).await;
        assert_eq!(
            body["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
    }
}
