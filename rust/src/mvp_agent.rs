//! mvp_agent.rs —— 100 行以内的 LLM Agent(Rust 版):JSON 用 serde_json,HTTP(S) 用 ureq,超时用 wait-timeout。用法:export LLM_API_KEY=sk-... && cargo run --quiet -- [任务]
use serde_json::{json, Map, Value};
use std::io::{BufRead, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{env, fs};
use wait_timeout::ChildExt;
const SYS: &str = "你是AI助手,可用工具:
1. tool_bash — 执行系统命令(bash)
2. tool_read — 读取文件
3. tool_write — 写入文件

请逐步思考。当需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。";
struct Cfg { key: String, url: String, model: String, turns: usize, timeout: f64, tool_to: f64, max_chars: usize, max_hist: usize }
fn e(k: &str, d: &str) -> String { env::var(k).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| d.into()) }
fn en(k: &str, d: f64) -> f64 { env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d) }
fn num(x: f64) -> String { if x.fract() == 0.0 { format!("{}", x as i64) } else { format!("{x}") } }
fn type_name(v: &Value) -> &'static str { match v { Value::Null => "NoneType", Value::Bool(_) => "bool", Value::Number(_) => "number", Value::String(_) => "str", Value::Array(_) => "list", _ => "dict" } }
fn config() -> Cfg { Cfg { key: e("LLM_API_KEY", &e("DEEPSEEK_API_KEY", &e("OPENAI_API_KEY", ""))), url: e("LLM_BASE_URL", "https://api.deepseek.com/v1").trim_end_matches('/').into(), model: e("LLM_MODEL", "deepseek-chat"), turns: en("LLM_MAX_TURNS", 15.0) as usize, timeout: en("LLM_TIMEOUT", 120.0), tool_to: en("LLM_TOOL_TIMEOUT", 60.0), max_chars: en("LLM_MAX_TOOL_CHARS", 16000.0) as usize, max_hist: (en("LLM_MAX_HISTORY", 40.0) as usize).max(1) } }
fn schemas() -> Value { let p = |d: &str| json!({"type": "string", "description": d}); json!([{"type":"function","function":{"name":"tool_bash","description":"执行系统命令","parameters":{"type":"object","properties":{"cmd":p("命令")},"required":["cmd"],"additionalProperties":false}}},{"type":"function","function":{"name":"tool_read","description":"读取文件","parameters":{"type":"object","properties":{"fp":p("文件路径")},"required":["fp"],"additionalProperties":false}}},{"type":"function","function":{"name":"tool_write","description":"写入文件","parameters":{"type":"object","properties":{"fp":p("文件路径"),"data":p("内容")},"required":["fp","data"],"additionalProperties":false}}}]) }
fn bash(cmd: &str, t: f64) -> String {
    let prog = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "sh" };
    let mut c = match Command::new(prog).arg("-c").arg(cmd).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() { Ok(c) => c, Err(e) => return format!("[错误]{e}") };
    let (mut so, mut se) = (c.stdout.take().unwrap(), c.stderr.take().unwrap());
    let ho = std::thread::spawn(move || { let mut v = Vec::new(); let _ = so.read_to_end(&mut v); v });
    let he = std::thread::spawn(move || { let mut v = Vec::new(); let _ = se.read_to_end(&mut v); v });
    let st = match c.wait_timeout(Duration::from_secs_f64(t)) { Ok(Some(s)) => s, Ok(None) => { let _ = c.kill(); let _ = c.wait(); return format!("[错误]命令超时(>{}s)", num(t)); }, Err(e) => return format!("[错误]{e}") };
    let mut out = String::from_utf8_lossy(&ho.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&he.join().unwrap_or_default()).into_owned();
    if !err.is_empty() { out.push_str(&format!("\n[stderr]\n{err}")); }
    if st.code().unwrap_or(0) != 0 { out.push_str(&format!("\n[exit:{}]", st.code().unwrap_or(0))); }
    let out = out.trim().to_string();
    if out.is_empty() { "(无输出)".into() } else { out }
}
fn read_f(fp: &str) -> Result<String, String> { fs::read(fp).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("OSError: {e}")) }
fn write_f(fp: &str, data: &str) -> Result<String, String> {
    if let Some(d) = Path::new(fp).parent() { fs::create_dir_all(d).map_err(|e| format!("OSError: {e}"))?; }
    fs::write(fp, data).map_err(|e| format!("OSError: {e}"))?; Ok(format!("OK—写入{}字符", data.chars().count()))
}
fn call(name: &str, f: &Value, to: f64) -> Result<String, String> {
    let raw = f.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
    let args: Value = serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw }).map_err(|e| format!("JSONDecodeError: {e}"))?;
    if !args.is_object() { return Err(format!("ValueError: arguments 必须是 JSON 对象,收到 {}", type_name(&args))); }
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| format!("TypeError: {k}() missing 1 required argument"));
    match name { "tool_bash" => Ok(bash(&s("cmd")?, to)), "tool_read" => read_f(&s("fp")?), "tool_write" => write_f(&s("fp")?, &s("data")?), _ => Err(format!("LookupError: 未知工具:{name}")) }
}
fn run_tool(tc: &Value, cfg: &Cfg) -> String {
    let f = tc.get("function").cloned().unwrap_or(Value::Null);
    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let mut out = call(name, &f, cfg.tool_to).unwrap_or_else(|e| format!("[错误]工具 {} 调用失败:{e}", if name.is_empty() { "?" } else { name }));
    let n = out.chars().count();
    if n > cfg.max_chars { out = format!("{}\n…[已截断,原始 {n} 字符]", out.chars().take(cfg.max_chars).collect::<String>()); }
    out
}
fn trim(ctx: &mut Vec<Value>, max: usize) {
    if ctx.len() <= max + 1 { return; }
    let mut cut = ctx.len() - max;
    while cut < ctx.len() && ctx[cut]["role"].as_str() == Some("tool") { cut += 1; }
    let tail = ctx.split_off(cut); ctx.truncate(1); ctx.extend(tail);
}
fn reason(c: u16) -> &'static str { match c { 400 => "Bad Request", 401 => "Unauthorized", 403 => "Forbidden", 404 => "Not Found", 408 => "Request Timeout", 413 => "Payload Too Large", 429 => "Too Many Requests", 500 => "Internal Server Error", 502 => "Bad Gateway", 503 => "Service Unavailable", _ => "" } }
fn chat(cfg: &Cfg, msgs: &[Value]) -> Result<Value, String> {
    let body = json!({"model": cfg.model, "messages": msgs, "tools": schemas()}).to_string();
    let req = ureq::post(&format!("{}/chat/completions", cfg.url)).set("Content-Type", "application/json; charset=utf-8").set("Authorization", &format!("Bearer {}", cfg.key)).set("User-Agent", "mvp-agent/1.0").timeout(Duration::from_secs_f64(cfg.timeout));
    let (code, text) = match req.send_string(&body) { Ok(r) => (200, r.into_string().map_err(|e| e.to_string())?), Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()), Err(e) => return Err(format!("网络错误:{e}")) };
    if !(200..300).contains(&code) { return Err(format!("HTTP {} {}: {}", code, reason(code), text.chars().take(1000).collect::<String>())); }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("网络错误:{e}"))?;
    v.get("choices").and_then(|c| c.get(0)).and_then(|c| c.get("message")).cloned().ok_or_else(|| "网络错误:响应缺少 choices[0].message".to_string())
}
fn agent_loop(ctx: &mut Vec<Value>, input: &str, cfg: &Cfg) -> String {
    ctx.push(json!({"role": "user", "content": input}));
    for _ in 0..cfg.turns {
        trim(ctx, cfg.max_hist);
        let m = match chat(cfg, ctx) { Ok(m) => m, Err(e) => { let l = e.to_lowercase(); return format!("[错误]{}:{e}", if l.contains("context") || l.contains("token") { "上下文过长,请重开会话" } else { "API失败" }); } };
        let mut a = Map::new(); for k in ["role", "content", "tool_calls"] { if let Some(v) = m.get(k) { a.insert(k.into(), v.clone()); } } ctx.push(Value::Object(a));
        let calls = m.get("tool_calls").and_then(|c| c.as_array()).cloned().unwrap_or_default();
        if calls.is_empty() { return m.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(); }
        for tc in &calls {
            let f = tc.get("function").cloned().unwrap_or(Value::Null); eprintln!("[tool_call]{}({})", f.get("name").and_then(|v| v.as_str()).unwrap_or("null"), f.get("arguments").and_then(|v| v.as_str()).unwrap_or("null"));
            ctx.push(json!({"role": "tool", "tool_call_id": tc.get("id").cloned().unwrap_or(Value::Null), "content": run_tool(tc, cfg)}));
        }
    }
    format!("[Agent]防止死循环,达到最大轮次({}),中断", cfg.turns)
}
fn main() {
    let cfg = config(); if cfg.key.is_empty() { eprintln!("错误:请设置LLM_API_KEY"); std::process::exit(1); }
    let mut ctx = vec![json!({"role": "system", "content": SYS})];
    let args: Vec<String> = env::args().skip(1).collect(); if !args.is_empty() { println!("助手>{}", agent_loop(&mut ctx, &args.join(" "), &cfg)); return; }
    eprintln!("智能助手 {} 就绪。输入消息(或'exit'退出)。\n", cfg.model);
    for line in std::io::stdin().lock().lines() {
        let inp = line.unwrap_or_default().trim().to_string(); if inp.is_empty() { continue; }
        if inp.eq_ignore_ascii_case("exit") || inp.eq_ignore_ascii_case("quit") { println!("再见。"); return; }
        eprintln!("处理请求:{inp}");
        println!("\n助手>{}\n", agent_loop(&mut ctx, &inp, &cfg));
    }
    println!("\n再见。");
}
#[cfg(test)] mod tests;
