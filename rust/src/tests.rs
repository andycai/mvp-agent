//! 离线测试:不联网、不需要真实 API Key。
//! 端到端用本地 TcpListener 假服务,并在每次请求上断言历史合法。
use super::json;
use super::*;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Handler = Arc<dyn Fn(usize, &Json) -> (u16, String) + Send + Sync>;

fn tmp_dir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("mvp-agent-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

fn cfg(base_url: &str) -> Config {
    Config {
        api_key: "test-key".into(),
        base_url: base_url.trim_end_matches('/').to_string(),
        model: "fake-model".into(),
        max_turns: 15,
        request_timeout: 20.0,
        tool_timeout: 10.0,
        max_tool_chars: 16000,
        max_history: 40,
    }
}

fn tc(id: &str, name: &str, args: &str) -> Json {
    obj(vec![
        ("id", s(id)),
        ("type", s("function")),
        ("function", obj(vec![("name", s(name)), ("arguments", s(args))])),
    ])
}

fn msg_calls(calls: Vec<Json>) -> Json {
    obj(vec![
        ("role", s("assistant")),
        ("content", Json::Null),
        ("tool_calls", Json::Arr(calls)),
    ])
}

fn msg_text(t: &str) -> Json {
    obj(vec![("role", s("assistant")), ("content", s(t))])
}

fn response(m: Json) -> String {
    obj(vec![("choices", Json::Arr(vec![obj(vec![("message", m)])]))]).to_string()
}

/// 历史合法性:首条 system;每个 tool_call 恰好一条配对 tool 响应。
fn legality(msgs: &[Json]) -> Vec<String> {
    let mut errs = Vec::new();
    if msgs.is_empty() {
        errs.push("空 messages".into());
        return errs;
    }
    if get_str(&msgs[0], "role") != Some("system") {
        errs.push("首条不是 system".into());
    }
    let mut pending: Vec<String> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        match get_str(m, "role") {
            Some("assistant") => {
                if !pending.is_empty() {
                    errs.push(format!("assistant 之前仍有未配对 {:?}", pending));
                }
                pending.clear();
                if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                    for t in tcs {
                        pending.push(get_str(t, "id").unwrap_or("").to_string());
                    }
                }
            }
            Some("tool") => {
                let id = get_str(m, "tool_call_id").unwrap_or("").to_string();
                match pending.iter().position(|p| *p == id) {
                    Some(p) => {
                        pending.remove(p);
                    }
                    None => errs.push(format!("孤儿 tool 响应 {} @{}", id, i)),
                }
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        errs.push(format!("结尾未配对 {:?}", pending));
    }
    errs
}

fn find_sub(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn content_length(head: &[u8]) -> usize {
    let t = String::from_utf8_lossy(head).to_lowercase();
    for line in t.split("\r\n") {
        if let Some(v) = line.strip_prefix("content-length:") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// 起一个最小 HTTP 假服务;返回 base_url 与它收到的所有请求体。
fn spawn_mock(h: Handler) -> (String, Arc<Mutex<Vec<Json>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Json>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut sock = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let mut buf: Vec<u8> = Vec::new();
            let mut tmp = [0u8; 8192];
            let mut head_end: Option<usize> = None;
            loop {
                let n = match sock.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                buf.extend_from_slice(&tmp[..n]);
                if head_end.is_none() {
                    head_end = find_sub(&buf, b"\r\n\r\n").map(|p| p + 4);
                }
                if let Some(he) = head_end {
                    if buf.len() >= he + content_length(&buf[..he]) {
                        break;
                    }
                }
            }
            let he = head_end.unwrap_or(buf.len()).min(buf.len());
            let body = String::from_utf8_lossy(&buf[he..]).to_string();
            let req = json::parse(&body).unwrap_or(Json::Null);
            let idx = {
                let mut v = seen2.lock().unwrap();
                v.push(req.clone());
                v.len() - 1
            };
            let (code, resp) = h(idx, &req);
            let reason = if code == 200 { "OK" } else { "Bad Request" };
            let out = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                code,
                reason,
                resp.len(),
                resp
            );
            let _ = sock.write_all(out.as_bytes());
            let _ = sock.flush();
        }
    });
    (format!("http://127.0.0.1:{}", port), seen)
}

// ---------------- JSON ----------------

#[test]
fn json_handles_escapes_unicode_and_surrogates() {
    let src = "{\"中\": \"文\\n\\\"引\\\"\", \"emoji\": \"\\ud83d\\ude00\", \"n\": [1, 2.5, true, null]}";
    let v = json::parse(src).unwrap();
    assert_eq!(get_str(&v, "中"), Some("文\n\"引\""));
    assert_eq!(get_str(&v, "emoji"), Some("😀"));
    assert_eq!(v.get("n").unwrap().as_array().unwrap().len(), 4);
}

#[test]
fn json_rejects_bad_input() {
    assert!(json::parse("{} x").is_err());
    assert!(json::parse("").is_err());
    assert!(json::parse("{'a':1}").is_err());
    assert!(json::parse("{\"a\":}").is_err());
}

#[test]
fn json_numbers_keep_python_types_and_spacing() {
    assert_eq!(json::parse("1").unwrap().py_type_name(), "int");
    assert_eq!(json::parse("1.5").unwrap().py_type_name(), "float");
    assert_eq!(json::parse("-3").unwrap().py_type_name(), "int");
    let d = json::parse("{\"a\": 1, \"b\": \"x\"}").unwrap().to_string();
    assert_eq!(d, "{\"a\": 1, \"b\": \"x\"}");
    assert_eq!(json::parse("[1, 2]").unwrap().to_string(), "[1, 2]");
}

// ---------------- 工具 ----------------

#[test]
fn schemas_are_complete() {
    let schemas = tool_schemas();
    assert_eq!(schemas.len(), 3);
    let names: Vec<&str> = schemas
        .iter()
        .map(|s| s.get("function").unwrap().get("name").unwrap().as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["tool_bash", "tool_read", "tool_write"]);
    for s in &schemas {
        let p = s.get("function").unwrap().get("parameters").unwrap();
        assert_eq!(p.get("additionalProperties"), Some(&Json::Bool(false)));
        assert_eq!(get_str(p, "type"), Some("object"));
    }
}

#[test]
fn bash_captures_stderr_and_exit_code() {
    let got = tool_bash("echo hello-stdout; echo oops >&2; exit 3", 10.0).unwrap();
    assert_eq!(got, "hello-stdout\n\n[stderr]\noops\n\n[exit:3]");
}

#[test]
fn bash_empty_output_placeholder() {
    assert_eq!(tool_bash("true", 10.0).unwrap(), "(无输出)");
}

#[test]
fn bash_timeout_message_matches_python() {
    assert_eq!(tool_bash("sleep 5", 0.3).unwrap(), "[错误]命令超时(>0.3s)");
    assert_eq!(tool_bash("sleep 5", 2.0).unwrap(), "[错误]命令超时(>2s)");
}

#[test]
fn read_missing_file_is_a_tool_error() {
    let c = cfg("http://127.0.0.1:1");
    let fp = format!("{}/definitely-missing", tmp_dir("missing"));
    let got = run_tool(&tc("c", "tool_read", &format!("{{\"fp\":\"{}\"}}", fp)), &c);
    assert!(got.starts_with("[错误]工具 tool_read 调用失败:OSError:"), "{}", got);
}

#[test]
fn write_creates_parents_and_counts_chars() {
    let fp = format!("{}/a/b/c.txt", tmp_dir("write"));
    let args = json::parse(&format!("{{\"fp\":\"{}\",\"data\":\"中文abc\"}}", fp)).unwrap();
    assert_eq!(call_tool("tool_write", &args, 10.0).unwrap(), "OK—写入5字符");
    assert_eq!(fs::read_to_string(&fp).unwrap(), "中文abc");
}

#[test]
fn unknown_tool_and_bad_arguments_stay_paired() {
    let c = cfg("http://127.0.0.1:1");
    let got = run_tool(&tc("c", "nope", "{}"), &c);
    assert!(got.contains("未知工具:nope"), "{}", got);
    assert!(got.starts_with("[错误]工具 nope 调用失败:LookupError:"), "{}", got);

    let got = run_tool(&tc("c", "tool_bash", "not-json"), &c);
    assert!(got.starts_with("[错误]工具 tool_bash 调用失败:JSONDecodeError:"), "{}", got);

    let got = run_tool(&tc("c", "tool_bash", "[1,2]"), &c);
    assert!(got.contains("必须是 JSON 对象,收到 list"), "{}", got);

    let got = run_tool(&tc("c", "tool_bash", ""), &c);
    assert!(got.starts_with("[错误]工具 tool_bash 调用失败:TypeError:"), "{}", got);
}

#[test]
fn truncation_counts_chars_not_bytes() {
    let fp = format!("{}/big.txt", tmp_dir("trunc"));
    fs::write(&fp, "中".repeat(20000)).unwrap();
    // 截断发生在 run_tool(工具结果的统一出口),不是工具函数内部
    let got = run_tool(&tc("c", "tool_read", &format!("{{\"fp\":\"{}\"}}", fp)), &cfg("http://127.0.0.1:1"));
    assert!(got.ends_with("[已截断,原始 20000 字符]"));
    let head = got.split("\n…[已截断").next().unwrap();
    assert_eq!(head.chars().count(), 16000);
}

// ---------------- 历史裁剪 ----------------

#[test]
fn trim_keeps_system_and_pairing() {
    let mut ctx = vec![obj(vec![("role", s("system"))])];
    for i in 0..10 {
        ctx.push(obj(vec![("role", s("user")), ("content", s("u"))]));
        ctx.push(msg_calls(vec![tc(&format!("c{}", i), "tool_bash", "{}")]));
        ctx.push(obj(vec![
            ("role", s("tool")),
            ("tool_call_id", s(&format!("c{}", i))),
            ("content", s("ok")),
        ]));
    }
    let out = trim(&ctx, 4);
    assert_eq!(get_str(&out[0], "role"), Some("system"));
    assert_ne!(get_str(&out[1], "role"), Some("tool"));
    assert!(out.len() <= 4 + 3, "裁剪后 {} 条", out.len());
    assert!(legality(&out).is_empty(), "{:?}", legality(&out));
}

#[test]
fn trim_small_history_is_untouched() {
    let ctx = vec![
        obj(vec![("role", s("system"))]),
        obj(vec![("role", s("user")), ("content", s("x"))]),
    ];
    assert_eq!(trim(&ctx, 40).len(), 2);
}

#[test]
fn trim_does_not_run_off_the_end() {
    let mut ctx = vec![obj(vec![("role", s("system"))])];
    for _ in 0..5 {
        ctx.push(obj(vec![
            ("role", s("tool")),
            ("tool_call_id", s("x")),
            ("content", s("r")),
        ]));
    }
    let out = trim(&ctx, 1);
    assert_eq!(get_str(&out[0], "role"), Some("system"));
}

// ---------------- HTTP ----------------

#[test]
fn chat_keeps_http_error_body() {
    let h: Handler = Arc::new(|_, _| (400, "{\"error\":{\"message\":\"boom-detail\"}}".to_string()));
    let (url, _seen) = spawn_mock(h);
    let err = chat(&cfg(&url), &[obj(vec![("role", s("system"))])]).unwrap_err();
    assert!(err.starts_with("HTTP 400 Bad Request:"), "{}", err);
    assert!(err.contains("boom-detail"), "{}", err);
}

#[test]
fn chat_reports_network_error() {
    let err = chat(&cfg("http://127.0.0.1:1"), &[]).unwrap_err();
    assert!(err.starts_with("网络错误:"), "{}", err);
}

// ---------------- 端到端 ----------------

#[test]
fn agent_loop_end_to_end_keeps_history_legal() {
    let dir = tmp_dir("e2e");
    let out_fp = format!("{}/o.txt", dir);
    let h: Handler = Arc::new(move |_, req| {
        let msgs = req.get("messages").and_then(|v| v.as_array()).unwrap_or(&[]);
        let n = msgs
            .iter()
            .filter(|m| get_str(m, "role") == Some("assistant") && m.get("tool_calls").is_some())
            .count();
        let m = match n {
            0 => msg_calls(vec![
                tc("call_1", "tool_bash", "{\"cmd\":\"echo hi\"}"),
                tc("call_2", "nope", "{}"),
                tc("call_3", "tool_bash", "not-json"),
                tc("call_4", "tool_write", &format!("{{\"fp\":\"{}\",\"data\":\"x\"}}", out_fp)),
            ]),
            1 => msg_calls(vec![tc(
                "call_5",
                "tool_read",
                &format!("{{\"fp\":\"{}\"}}", out_fp),
            )]),
            _ => msg_text("ALL-OK"),
        };
        (200, response(m))
    });
    let (url, seen) = spawn_mock(h);
    let mut ctx = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    assert_eq!(agent_loop(&cfg(&url), "任务", &mut ctx), "ALL-OK");

    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    for (i, b) in bodies.iter().enumerate() {
        let msgs = b.get("messages").and_then(|v| v.as_array()).unwrap();
        let errs = legality(msgs);
        assert!(errs.is_empty(), "第 {} 次请求历史非法:{:?}", i, errs);
    }
    let tools = ctx.iter().filter(|m| get_str(m, "role") == Some("tool")).count();
    assert_eq!(tools, 5);
    assert!(legality(&ctx).is_empty());
}

#[test]
fn agent_loop_classifies_context_overflow() {
    let h: Handler = Arc::new(|_, _| {
        (
            400,
            "{\"error\":{\"message\":\"maximum context length is 8192 tokens\"}}".to_string(),
        )
    });
    let (url, _seen) = spawn_mock(h);
    let mut ctx = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    let got = agent_loop(&cfg(&url), "任务", &mut ctx);
    assert!(got.starts_with("[错误]上下文过长,请重开会话:"), "{}", got);
}

#[test]
fn agent_loop_reports_plain_api_failure() {
    let h: Handler = Arc::new(|_, _| (401, "{\"error\":{\"message\":\"bad key\"}}".to_string()));
    let (url, _seen) = spawn_mock(h);
    let mut ctx = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    let got = agent_loop(&cfg(&url), "任务", &mut ctx);
    assert!(got.starts_with("[错误]API失败:HTTP 401"), "{}", got);
}

#[test]
fn agent_loop_stops_at_max_turns() {
    let h: Handler = Arc::new(|_, _| {
        (
            200,
            response(msg_calls(vec![tc("c", "tool_bash", "{\"cmd\":\"true\"}")])),
        )
    });
    let (url, seen) = spawn_mock(h);
    let mut c = cfg(&url);
    c.max_turns = 3;
    let mut ctx = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    let got = agent_loop(&c, "任务", &mut ctx);
    assert!(got.contains("达到最大轮次(3)"), "{}", got);
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert!(legality(&ctx).is_empty());
}

#[test]
fn agent_loop_history_stays_bounded() {
    let h: Handler = Arc::new(|i, _req| {
        let m = if i >= 24 {
            msg_text("TRIM-OK")
        } else {
            msg_calls(vec![tc(&format!("c{}", i), "tool_bash", "{\"cmd\":\"true\"}")])
        };
        (200, response(m))
    });
    let (url, seen) = spawn_mock(h);
    let mut c = cfg(&url);
    c.max_history = 6;
    c.max_turns = 40;
    let mut ctx = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    assert_eq!(agent_loop(&c, "任务", &mut ctx), "TRIM-OK");

    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 25);
    let mut peak = 0;
    for b in bodies.iter() {
        let msgs = b.get("messages").and_then(|v| v.as_array()).unwrap();
        peak = peak.max(msgs.len());
        let errs = legality(msgs);
        assert!(errs.is_empty(), "{:?}", errs);
    }
    assert!(peak <= 9, "历史未被有界裁剪,峰值 {} 条", peak);
}
