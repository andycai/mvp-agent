//! 离线测试:不联网、不需要真实 API Key。端到端用本地 TcpListener 假服务,并在每次请求上断言历史合法。
use super::*;
use serde_json::{json, Value};
use std::io::Write as _; // Read 由 super::* 提供
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Handler = Arc<dyn Fn(usize, &Value) -> (u16, String) + Send + Sync>;

fn tmp_dir(tag: &str) -> String {
    let d = env::temp_dir().join(format!("mvp-agent-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}
fn cfg(base: &str) -> Cfg {
    Cfg { key: "test-key".into(), url: base.trim_end_matches('/').to_string(), model: "fake-model".into(), turns: 15, timeout: 20.0, tool_to: 10.0, max_chars: 16000, max_hist: 40 }
}
fn tc(id: &str, name: &str, args: &str) -> Value {
    json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
}
fn msg_calls(calls: Vec<Value>) -> Value {
    json!({"role": "assistant", "content": null, "tool_calls": calls})
}
fn msg_text(t: &str) -> Value {
    json!({"role": "assistant", "content": t})
}
fn response(m: Value) -> String {
    json!({"choices": [{"message": m}]}).to_string()
}
fn roles(msgs: &[Value]) -> Vec<&str> {
    msgs.iter().map(|m| m["role"].as_str().unwrap_or("")).collect()
}
/// 历史合法性:首条 system;每个 tool_call 恰好一条配对 tool 响应。
fn legality(msgs: &[Value]) -> Vec<String> {
    let mut errs = Vec::new();
    if msgs.is_empty() {
        return vec!["空 messages".into()];
    }
    if msgs[0]["role"].as_str() != Some("system") {
        errs.push("首条不是 system".into());
    }
    let mut pending: Vec<String> = Vec::new();
    for (i, m) in msgs.iter().enumerate() {
        match m["role"].as_str() {
            Some("assistant") => {
                if !pending.is_empty() {
                    errs.push(format!("assistant 之前仍有未配对 {pending:?}"));
                }
                pending.clear();
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for t in tcs {
                        pending.push(t["id"].as_str().unwrap_or("").to_string());
                    }
                }
            }
            Some("tool") => {
                let id = m["tool_call_id"].as_str().unwrap_or("").to_string();
                match pending.iter().position(|p| *p == id) {
                    Some(p) => drop(pending.remove(p)),
                    None => errs.push(format!("孤儿 tool 响应 {id} @{i}")),
                }
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        errs.push(format!("结尾未配对 {pending:?}"));
    }
    errs
}
fn find_sub(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}
fn content_length(head: &[u8]) -> usize {
    for line in String::from_utf8_lossy(head).to_lowercase().split("\r\n") {
        if let Some(v) = line.strip_prefix("content-length:") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}
/// 起一个最小 HTTP 假服务;返回 base_url 与它收到的所有请求体。
fn spawn_mock(h: Handler) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut sock = match stream {
                Ok(s) => s,
                Err(_) => break,
            };
            let (mut buf, mut tmp, mut head_end) = (Vec::new(), [0u8; 8192], None);
            loop {
                let n = match sock.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
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
            let req = serde_json::from_str(&String::from_utf8_lossy(&buf[he..])).unwrap_or(Value::Null);
            let idx = {
                let mut v = seen2.lock().unwrap();
                v.push(req.clone());
                v.len() - 1
            };
            let (code, resp) = h(idx, &req);
            let out = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                code, if code == 200 { "OK" } else { "Bad Request" }, resp.len(), resp
            );
            let _ = sock.write_all(out.as_bytes());
            let _ = sock.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

#[test]
fn schemas_are_complete() {
    let s = schemas();
    let arr = s.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(roles(arr), vec![""; 3]);
    let names: Vec<&str> = arr.iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["tool_bash", "tool_read", "tool_write"]);
    for t in arr {
        let p = &t["function"]["parameters"];
        assert_eq!(p["additionalProperties"], json!(false));
        assert_eq!(p["type"], json!("object"));
    }
}
#[test]
fn bash_captures_stderr_and_exit_code() {
    assert_eq!(bash("echo hello-stdout; echo oops >&2; exit 3", 10.0), "hello-stdout\n\n[stderr]\noops\n\n[exit:3]");
}
#[test]
fn bash_empty_output_placeholder() {
    assert_eq!(bash("true", 10.0), "(无输出)");
}
#[test]
fn bash_timeout_message_matches_python() {
    assert_eq!(bash("sleep 5", 0.3), "[错误]命令超时(>0.3s)");
    assert_eq!(bash("sleep 5", 2.0), "[错误]命令超时(>2s)");
}
#[test]
fn bash_does_not_deadlock_on_large_output() {
    let out = bash("yes 中文 | head -c 200000", 20.0);
    assert!(out.chars().count() > 60000, "大输出被截断或死锁:{}", out.chars().count());
}
#[test]
fn read_missing_file_is_a_tool_error() {
    let fp = format!("{}/definitely-missing", tmp_dir("missing"));
    let got = run_tool(&tc("c", "tool_read", &json!({"fp": fp}).to_string()), &cfg("http://127.0.0.1:1"));
    assert!(got.starts_with("[错误]工具 tool_read 调用失败:OSError:"), "{got}");
}
#[test]
fn write_creates_parents_and_counts_chars() {
    let fp = format!("{}/a/b/c.txt", tmp_dir("write"));
    let got = run_tool(&tc("c", "tool_write", &json!({"fp": fp, "data": "中文abc"}).to_string()), &cfg("http://127.0.0.1:1"));
    assert_eq!(got, "OK—写入5字符");
    assert_eq!(fs::read_to_string(&fp).unwrap(), "中文abc");
}
#[test]
fn unknown_tool_and_bad_arguments_stay_paired() {
    let c = cfg("http://127.0.0.1:1");
    let got = run_tool(&tc("c", "nope", "{}"), &c);
    assert!(got.contains("未知工具:nope") && got.starts_with("[错误]工具 nope 调用失败:LookupError:"), "{got}");
    let got = run_tool(&tc("c", "tool_bash", "not-json"), &c);
    assert!(got.starts_with("[错误]工具 tool_bash 调用失败:JSONDecodeError:"), "{got}");
    let got = run_tool(&tc("c", "tool_bash", "[1,2]"), &c);
    assert!(got.contains("必须是 JSON 对象,收到 list"), "{got}");
    let got = run_tool(&tc("c", "tool_bash", ""), &c);
    assert!(got.starts_with("[错误]工具 tool_bash 调用失败:TypeError:"), "{got}");
}
#[test]
fn truncation_counts_chars_not_bytes() {
    let fp = format!("{}/big.txt", tmp_dir("trunc"));
    fs::write(&fp, "中".repeat(20000)).unwrap();
    let got = run_tool(&tc("c", "tool_read", &json!({"fp": fp}).to_string()), &cfg("http://127.0.0.1:1"));
    assert!(got.ends_with("[已截断,原始 20000 字符]"));
    assert_eq!(got.split("\n…[已截断").next().unwrap().chars().count(), 16000);
}
#[test]
fn trim_keeps_system_and_pairing() {
    let mut ctx = vec![json!({"role": "system"})];
    for i in 0..10 {
        ctx.push(json!({"role": "user", "content": "u"}));
        ctx.push(msg_calls(vec![tc(&format!("c{i}"), "tool_bash", "{}")]));
        ctx.push(json!({"role": "tool", "tool_call_id": format!("c{i}"), "content": "ok"}));
    }
    let mut c = cfg("http://127.0.0.1:1");
    c.max_hist = 4;
    trim(&mut ctx, c.max_hist);
    assert_eq!(ctx[0]["role"], json!("system"));
    assert_ne!(ctx[1]["role"], json!("tool"));
    assert!(ctx.len() <= 4 + 3, "裁剪后 {} 条", ctx.len());
    assert_eq!(legality(&ctx), Vec::<String>::new());
}
#[test]
fn trim_small_history_is_untouched() {
    let mut ctx = vec![json!({"role": "system"}), json!({"role": "user", "content": "x"})];
    trim(&mut ctx, 40);
    assert_eq!(ctx.len(), 2);
}
#[test]
fn trim_does_not_run_off_the_end() {
    let mut ctx = vec![json!({"role": "system"})];
    for _ in 0..5 {
        ctx.push(json!({"role": "tool", "tool_call_id": "x", "content": "r"}));
    }
    trim(&mut ctx, 1);
    assert_eq!(ctx[0]["role"], json!("system"));
}
#[test]
fn chat_keeps_http_error_body() {
    let (url, _s) = spawn_mock(Arc::new(|_, _| (400, r#"{"error":{"message":"boom-detail"}}"#.to_string())));
    let e = chat(&cfg(&url), &[json!({"role": "system"})]).unwrap_err();
    assert!(e.starts_with("HTTP 400 Bad Request:"), "{e}");
    assert!(e.contains("boom-detail"), "{e}");
}
#[test]
fn chat_reports_network_error() {
    let e = chat(&cfg("http://127.0.0.1:1"), &[]).unwrap_err();
    assert!(e.starts_with("网络错误:"), "{e}");
}
#[test]
fn agent_loop_end_to_end_keeps_history_legal() {
    let out_fp = format!("{}/o.txt", tmp_dir("e2e"));
    let h: Handler = Arc::new(move |_, req| {
        let msgs = req["messages"].as_array().cloned().unwrap_or_default();
        let n = msgs.iter().filter(|m| m["role"] == json!("assistant") && m["tool_calls"].is_array()).count();
        let m = match n {
            0 => msg_calls(vec![
                tc("call_1", "tool_bash", r#"{"cmd":"echo hi"}"#),
                tc("call_2", "nope", "{}"),
                tc("call_3", "tool_bash", "not-json"),
                tc("call_4", "tool_write", &json!({"fp": out_fp, "data": "x"}).to_string()),
            ]),
            1 => msg_calls(vec![tc("call_5", "tool_read", &json!({"fp": out_fp}).to_string())]),
            _ => msg_text("ALL-OK"),
        };
        (200, response(m))
    });
    let (url, seen) = spawn_mock(h);
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    assert_eq!(agent_loop(&mut ctx, "任务", &cfg(&url)), "ALL-OK");
    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    for (i, b) in bodies.iter().enumerate() {
        assert_eq!(legality(b["messages"].as_array().unwrap()), Vec::<String>::new(), "第 {i} 次请求历史非法");
    }
    assert_eq!(ctx.iter().filter(|m| m["role"] == json!("tool")).count(), 5);
    assert_eq!(legality(&ctx), Vec::<String>::new());
}
#[test]
fn agent_loop_classifies_context_overflow() {
    let (url, _s) = spawn_mock(Arc::new(|_, _| (400, r#"{"error":{"message":"maximum context length is 8192 tokens"}}"#.to_string())));
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    assert!(agent_loop(&mut ctx, "任务", &cfg(&url)).starts_with("[错误]上下文过长,请重开会话:"));
}
#[test]
fn agent_loop_reports_plain_api_failure() {
    let (url, _s) = spawn_mock(Arc::new(|_, _| (401, r#"{"error":{"message":"bad key"}}"#.to_string())));
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    assert!(agent_loop(&mut ctx, "任务", &cfg(&url)).starts_with("[错误]API失败:HTTP 401"));
}
#[test]
fn agent_loop_stops_at_max_turns() {
    let (url, seen) = spawn_mock(Arc::new(|_, _| (200, response(msg_calls(vec![tc("c", "tool_bash", r#"{"cmd":"true"}"#)])))));
    let mut c = cfg(&url);
    c.turns = 3;
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    assert!(agent_loop(&mut ctx, "任务", &c).contains("达到最大轮次(3)"));
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(legality(&ctx), Vec::<String>::new());
}
#[test]
fn agent_loop_history_stays_bounded() {
    let h: Handler = Arc::new(|i, _| {
        let m = if i >= 24 { msg_text("TRIM-OK") } else { msg_calls(vec![tc(&format!("c{i}"), "tool_bash", r#"{"cmd":"true"}"#)]) };
        (200, response(m))
    });
    let (url, seen) = spawn_mock(h);
    let mut c = cfg(&url);
    c.max_hist = 6;
    c.turns = 40;
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    assert_eq!(agent_loop(&mut ctx, "任务", &c), "TRIM-OK");
    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 25);
    let mut peak = 0;
    for b in bodies.iter() {
        let msgs = b["messages"].as_array().unwrap();
        peak = peak.max(msgs.len());
        assert_eq!(legality(msgs), Vec::<String>::new());
    }
    assert!(peak <= 9, "历史未被有界裁剪,峰值 {peak} 条");
}
#[test]
fn config_reads_env_with_fallbacks() {
    let saved: Vec<(&str, Option<String>)> = ["LLM_API_KEY", "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "LLM_MODEL", "LLM_MAX_HISTORY", "LLM_BASE_URL"]
        .iter()
        .map(|k| (*k, env::var(k).ok()))
        .collect();
    unsafe {
        env::remove_var("LLM_API_KEY");
        env::remove_var("OPENAI_API_KEY");
        env::set_var("DEEPSEEK_API_KEY", "dk");
        env::set_var("LLM_MODEL", "m1");
        env::set_var("LLM_MAX_HISTORY", "0");
        env::set_var("LLM_BASE_URL", "http://x/v1/");
    }
    let c = config();
    assert_eq!(c.key, "dk");
    assert_eq!(c.model, "m1");
    assert_eq!(c.max_hist, 1);
    assert_eq!(c.url, "http://x/v1");
    assert_eq!(c.turns, 15);
    for (k, v) in saved {
        unsafe {
            match v {
                Some(v) => env::set_var(k, v),
                None => env::remove_var(k),
            }
        }
    }
}
