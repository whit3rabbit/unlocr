use super::local::server_args;
use super::*;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;

/// The optional DeepSeek-OCR flags must reach the spawn args only when set,
/// and the no-flags baseline must stay byte-for-byte what it was. Mirrors the
/// EH-0002 "prove the flag reaches the subprocess" pattern, no network/spawn.
#[test]
fn server_args_adds_optional_flags_only_when_set() {
    let model = Path::new("/m/model.gguf");
    let mmproj = Path::new("/m/mmproj.gguf");

    // Test paths are ASCII, so to_string_lossy round-trips losslessly here;
    // the OsString return type matters only for non-UTF8 paths in the wild.
    let stringify = |v: Vec<OsString>| -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    };

    let base = stringify(server_args(model, mmproj, 8080, None, None));
    assert_eq!(
        base,
        vec![
            "-m",
            "/m/model.gguf",
            "--mmproj",
            "/m/mmproj.gguf",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
        ]
    );
    assert!(!base
        .iter()
        .any(|a| a == "--image-max-tokens" || a == "--chat-template"));

    let full = stringify(server_args(
        model,
        mmproj,
        8080,
        Some(1280),
        Some("deepseek-ocr"),
    ));
    // Each flag appears adjacent to its value.
    assert!(full.windows(2).any(|w| w == ["--image-max-tokens", "1280"]));
    assert!(full
        .windows(2)
        .any(|w| w == ["--chat-template", "deepseek-ocr"]));
}

#[test]
fn parses_content() {
    let resp = json!({ "choices": [{ "message": { "content": "# hi" } }] });
    assert_eq!(parse_completion(&resp).unwrap(), "# hi");
}

#[test]
fn rejects_bad_shape() {
    assert!(parse_completion(&json!({})).is_err());
    assert!(parse_completion(&json!({ "choices": [] })).is_err());
    // content present but wrong type
    let bad = json!({ "choices": [{ "message": { "content": 42 } }] });
    assert!(parse_completion(&bad).is_err());
}

/// RemoteEndpoint must POST to {base}/v1/chat/completions, send the bearer
/// token, and return the assistant text. Stub HTTP server captures the
/// Authorization header + request path so we lock both the routing and auth.
#[test]
fn remote_endpoint_sends_bearer_and_parses() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let resp_body = json!({ "choices": [{ "message": { "content": "# remote ok" } }] }).to_string();
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        resp_body.len(),
        resp_body,
    );

    // Capture the request line + Authorization header from one request.
    let (tx, rx) = mpsc::channel::<(String, Option<String>)>();
    std::thread::spawn(move || {
        let Ok(s) = listener.incoming().next().unwrap() else {
            return;
        };
        let mut reader = BufReader::new(s.try_clone().unwrap());
        let mut writer = s;
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok();
        let mut auth = None;
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            let t = line.trim_end_matches(['\r', '\n']);
            if t.is_empty() {
                break;
            }
            let lower = t.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("authorization:") {
                auth = Some(v.trim().to_string());
            }
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
        let _ = writer.write_all(http_response.as_bytes());
        let _ = tx.send((request_line.trim().to_string(), auth));
    });

    let ep = RemoteEndpoint {
        base_url: format!("http://127.0.0.1:{port}/"), // trailing slash must be trimmed
        api_key: Some("secret".to_string()),
        model: None,
    };
    let out = ep
        .ocr_image(
            "<|grounding|>x",
            "data:image/png;base64,AAAA",
            64,
            None,
            None,
            None,
        )
        .expect("remote ocr");
    assert_eq!(out.text, "# remote ok");

    let (request_line, auth) = rx.recv().expect("stub recorded request");
    assert_eq!(request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(auth.as_deref(), Some("bearer secret"));
}

/// A `model` set on RemoteEndpoint must land in the request body (gateways
/// like litellm/vLLM require it); when unset, no `"model"` key is sent (a bare
/// llama-server would reject an empty/unknown model). Stub captures the body.
#[test]
fn remote_endpoint_injects_model_only_when_set() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(model: Option<String>) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(body);
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model,
        };
        ep.ocr_image("p", "data:image/png;base64,AAAA", 64, None, None, None)
            .expect("ocr");
        let body = rx.recv().expect("stub recorded body");
        serde_json::from_slice(&body).expect("body is json")
    }

    let with = run_once(Some("my-model".to_string()));
    assert_eq!(with["model"], json!("my-model"));

    let without = run_once(None);
    assert!(
        without.get("model").is_none(),
        "model key must be absent when unset"
    );
}

/// EH-0010 acceptance: prove `ocr_via_stream` fires `on_token` once per SSE
/// `data:` chunk and assembles the full text correctly.
///
/// The stub HTTP server returns a proper SSE body with `stream: true` semantics:
///   data: {"choices":[{"delta":{"content":"Hello"}}]}
///   data: {"choices":[{"delta":{"content":" world"}}]}
///   data: [DONE]
///
/// This is the real SSE wire format that llama-server sends. The test verifies:
///   1. on_token fires exactly twice, once per chunk.
///   2. The assembled return value equals the concatenation of both chunks.
///   3. Blank lines and [DONE] are silently skipped (not counted as tokens).
#[test]
fn sse_streaming_fires_on_token() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // Build the SSE body: two chunks then [DONE].
    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\r\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        // Serve a single connection with the SSE response, then exit.
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            // Drain the request headers + body.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    assert!(result.is_ok(), "ocr_via_stream failed: {:?}", result.err());
    let assembled = result.unwrap();

    // on_token must fire exactly once per data chunk (2 chunks, not 3 — [DONE] is not a token).
    assert_eq!(
        tokens,
        vec!["Hello".to_string(), " world".to_string()],
        "on_token fired with unexpected chunks: {tokens:?}"
    );
    // Assembled text must be the concatenation of both chunks.
    assert_eq!(
        assembled.text, "Hello world",
        "assembled text mismatch: {:?}",
        assembled.text
    );
    assert!(
        !assembled.truncated,
        "no finish_reason in the stub chunks; must not be flagged truncated"
    );
}

/// Non-SSE fallback: a server that ignores `stream: true` and replies with a
/// single plain JSON completion body must still yield its text (parsed like
/// ocr_via) rather than an empty string. Regression guard for the streaming
/// switch in ocr_pages, which otherwise silently dropped such responses.
#[test]
fn stream_falls_back_to_plain_json_completion() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // A normal (non-streaming) OpenAI chat-completion body, no `data:` framing.
    let json_body = r#"{"choices":[{"message":{"content":"plain json ok"}}]}"#;
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        json_body.len(),
        json_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok((sock, _)) = listener.accept() {
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    let assembled = result.expect("ocr_via_stream fallback failed");
    assert_eq!(assembled.text, "plain json ok", "fallback text mismatch");
    // The fallback delivers the whole body as one on_token call.
    assert_eq!(tokens, vec!["plain json ok".to_string()]);
}

/// A 2xx stream that carries a provider error chunk (`{"error":{"message":..}}`)
/// must surface as an Err carrying that message, not silently finish with the
/// partial text gathered so far. Regression guard for the bug where the SSE loop
/// only inspected `choices[0].delta.content` and let error chunks fall through,
/// making a failed run look successful with empty/partial output.
#[test]
fn sse_streaming_surfaces_provider_error() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    // A single error chunk (vLLM/OpenAI-style), then [DONE]. No content chunks.
    let sse_body = concat!(
        "data: {\"error\":{\"message\":\"Context length exceeded\"}}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let mut tokens: Vec<String> = Vec::new();
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        None,
        None,
        None,
        &mut |chunk: &str| {
            tokens.push(chunk.to_string());
            true
        },
        &|| false,
    );

    let err = result.expect_err("provider error chunk must surface as Err, not Ok");
    let msg = err.to_string();
    assert!(
        msg.contains("Context length exceeded"),
        "error message must carry the provider text, got: {msg}"
    );
    assert!(
        tokens.is_empty(),
        "no token should be emitted for an error chunk"
    );
}

/// `apply_sampling` must leave the body byte-for-byte unchanged on (None, None,
/// None) (the remote/vLLM baseline), and land each knob under its llama.cpp
/// field name when set. DRY implies the hardcoded `dry_allowed_length: 4`
/// companion. `dry_base` must reach the wire only when `dry_multiplier` is also
/// set (a base with DRY disabled is inert in llama.cpp). Pure.
#[test]
fn apply_sampling_sets_fields_only_when_set() {
    let baseline = json!({ "temperature": 0, "max_tokens": 64 });

    let mut body = baseline.clone();
    apply_sampling(&mut body, None, None, None);
    assert_eq!(body, baseline, "None/None/None must not touch the body");

    let mut body = baseline.clone();
    apply_sampling(&mut body, Some(1.1), Some(0.8), Some(2.0));
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert_eq!(body["dry_multiplier"], json!(0.8f32));
    assert_eq!(body["dry_allowed_length"], json!(4));
    assert_eq!(body["dry_base"], json!(2.0f32));

    let mut body = baseline.clone();
    apply_sampling(&mut body, None, Some(0.8), Some(2.0));
    assert!(body.get("repeat_penalty").is_none());
    assert_eq!(body["dry_multiplier"], json!(0.8f32));
    assert_eq!(body["dry_allowed_length"], json!(4));
    assert_eq!(body["dry_base"], json!(2.0f32));

    let mut body = baseline.clone();
    apply_sampling(&mut body, Some(1.1), None, None);
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert!(body.get("dry_multiplier").is_none());
    assert!(body.get("dry_allowed_length").is_none());

    // Critical gating case: dry_base supplied but dry_multiplier absent must be
    // dropped, since dry_base alone is inert in llama.cpp.
    let mut body = baseline;
    apply_sampling(&mut body, Some(1.1), None, Some(2.0));
    assert_eq!(body["repeat_penalty"], json!(1.1f32));
    assert!(body.get("dry_multiplier").is_none());
    assert!(body.get("dry_allowed_length").is_none());
    assert!(
        body.get("dry_base").is_none(),
        "dry_base must be dropped when dry_multiplier is None"
    );
}

/// The DRY fields must reach the wire only when `dry_multiplier` is set (a
/// remote endpoint that rejects llama.cpp-only fields must never see them).
/// Stub HTTP server captures the request body, mirroring
/// `remote_endpoint_injects_model_only_when_set`.
#[test]
fn ocr_image_sends_dry_fields_only_when_set() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(dry_multiplier: Option<f32>, dry_base: Option<f32>) -> serde_json::Value {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({ "choices": [{ "message": { "content": "ok" } }] }).to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(body);
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model: None,
        };
        ep.ocr_image(
            "p",
            "data:image/png;base64,AAAA",
            64,
            None,
            dry_multiplier,
            dry_base,
        )
        .expect("ocr");
        let body = rx.recv().expect("stub recorded body");
        serde_json::from_slice(&body).expect("body is json")
    }

    let with = run_once(Some(0.8), Some(2.0));
    assert_eq!(with["dry_multiplier"], json!(0.8f32));
    assert_eq!(with["dry_allowed_length"], json!(4));
    assert_eq!(with["dry_base"], json!(2.0f32));

    let without = run_once(None, None);
    assert!(
        without.get("dry_multiplier").is_none() && without.get("dry_allowed_length").is_none(),
        "DRY fields must be absent when unset"
    );

    // Critical gating case, exercised end-to-end through the stub HTTP server:
    // dry_base supplied but dry_multiplier absent must not reach the wire.
    let base_only = run_once(None, Some(2.0));
    assert!(
        base_only.get("dry_base").is_none(),
        "dry_base must be absent when dry_multiplier is None"
    );
}

/// `provider_error` extracts the message from the OpenAI/vLLM/SGLang error
/// object and returns None for a normal completion body. Pure, no network.
#[test]
fn provider_error_extracts_message() {
    assert_eq!(
        provider_error(&json!({"error": {"message": "boom", "code": 400}})),
        Some("boom".to_string())
    );
    assert_eq!(
        provider_error(&json!({"choices": [{"message": {"content": "ok"}}]})),
        None
    );
    assert_eq!(provider_error(&json!({})), None);
}

/// `finish_reason` extracts `choices[0].finish_reason` and returns None when
/// absent (e.g. a mid-stream delta chunk that hasn't finished yet). Pure, no
/// network. `OcrResult::truncated` is built from this returning `"length"`.
#[test]
fn finish_reason_extracts_value() {
    assert_eq!(
        finish_reason(&json!({"choices": [{"finish_reason": "length"}]})),
        Some("length".to_string())
    );
    assert_eq!(
        finish_reason(&json!({"choices": [{"finish_reason": "stop"}]})),
        Some("stop".to_string())
    );
    assert_eq!(
        finish_reason(&json!({"choices": [{"delta": {"content": "x"}}]})),
        None
    );
    assert_eq!(finish_reason(&json!({})), None);
}

/// A non-streaming completion whose `finish_reason` is `"length"` must be
/// flagged `truncated: true` (the model hit `max_tokens` without a natural
/// stop, the strongest available signal of a repetition loop); `"stop"` must
/// be `false`. Stub HTTP server, mirrors `remote_endpoint_sends_bearer_and_parses`.
#[test]
fn ocr_image_flags_truncated_from_finish_reason() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::mpsc;

    fn run_once(finish_reason: &str) -> OcrResult {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let port = listener.local_addr().unwrap().port();
        let resp_body = json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": finish_reason }]
        })
        .to_string();
        let http_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let Ok(s) = listener.incoming().next().unwrap() else {
                return;
            };
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut writer = s;
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).ok();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if let Some(v) = t.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(http_response.as_bytes());
            let _ = tx.send(());
        });

        let ep = RemoteEndpoint {
            base_url: format!("http://127.0.0.1:{port}"),
            api_key: None,
            model: None,
        };
        let out = ep
            .ocr_image("p", "data:image/png;base64,AAAA", 64, None, None, None)
            .expect("ocr");
        rx.recv().expect("stub served the request");
        out
    }

    assert!(
        run_once("length").truncated,
        "finish_reason length -> truncated"
    );
    assert!(
        !run_once("stop").truncated,
        "finish_reason stop -> not truncated"
    );
}

/// Same signal over the streaming path: `finish_reason: "length"` on the
/// terminal SSE chunk must set `OcrResult::truncated`. Mirrors
/// `sse_streaming_fires_on_token`'s stub server.
#[test]
fn sse_streaming_flags_truncated_from_finish_reason() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    let sse_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"loop\"}}]}\r\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\r\n",
        "data: [DONE]\r\n",
    );
    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
        sse_body.len(),
        sse_body,
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().unwrap().port();

    let http_resp_clone = http_response.clone();
    std::thread::spawn(move || {
        if let Ok(stream) = listener.accept() {
            let (sock, _) = stream;
            let mut reader = BufReader::new(sock.try_clone().expect("clone"));
            let mut writer = sock;
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let t = line.trim_end_matches(['\r', '\n']);
                if t.is_empty() {
                    break;
                }
                if t.to_ascii_lowercase().starts_with("content-length:") {
                    if let Some(v) = t.split_once(':').map(|x| x.1) {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = Read::read_exact(&mut reader, &mut body);
            let _ = writer.write_all(http_resp_clone.as_bytes());
        }
    });

    let base_url = format!("http://127.0.0.1:{port}");
    let result = ocr_via_stream(
        &base_url,
        None,
        None,
        "test prompt",
        "data:image/png;base64,AAAA",
        64,
        None,
        None,
        None,
        &mut |_chunk: &str| true,
        &|| false,
    );

    let out = result.expect("ocr_via_stream failed");
    assert_eq!(out.text, "loop");
    assert!(
        out.truncated,
        "finish_reason length on the terminal chunk must set truncated"
    );
}
