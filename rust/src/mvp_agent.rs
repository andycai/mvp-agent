//! mvp_agent —— 极简 LLM Agent(Rust 标准库实现,零第三方依赖)。
//! Rust 标准库不含 HTTP/TLS 与 JSON:HTTPS 通过调用系统 curl 子进程完成,
//! JSON 由 src/json.rs 自带的极简实现解析/序列化(见 README 的说明)。
//! 用法:export LLM_API_KEY=sk-... && cargo run --quiet -- [任务]
mod json;

#[cfg(test)]
mod tests;

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use json::Json;

const SYS_PROMPT: &str = "你是AI助手,可用工具:
1. tool_bash — 执行系统命令(bash)
2. tool_read — 读取文件
3. tool_write — 写入文件

请逐步思考。需要与文件系统交互、运行命令或执行代码等等时调用工具,完成后直接回复。";

/// 运行参数。集中在一个结构体里显式传递,测试无需改动进程环境变量。
#[derive(Clone, Debug)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub max_turns: usize,
    pub request_timeout: f64,
    pub tool_timeout: f64,
    pub max_tool_chars: usize,
    pub max_history: usize,
}

fn env_str(k: &str) -> Option<String> {
    env::var(k).ok().filter(|s| !s.is_empty())
}

fn env_usize(k: &str, d: usize) -> usize {
    env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn env_f64(k: &str, d: f64) -> f64 {
    env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

impl Config {
    pub fn from_env() -> Config {
        Config {
            api_key: env_str("LLM_API_KEY")
                .or_else(|| env_str("DEEPSEEK_API_KEY"))
                .or_else(|| env_str("OPENAI_API_KEY"))
                .unwrap_or_default(),
            base_url: env_str("LLM_BASE_URL")
                .unwrap_or_else(|| "https://api.deepseek.com/v1".into())
                .trim_end_matches('/')
                .to_string(),
            model: env_str("LLM_MODEL").unwrap_or_else(|| "deepseek-chat".into()),
            max_turns: env_usize("LLM_MAX_TURNS", 15),
            request_timeout: env_f64("LLM_TIMEOUT", 120.0),
            tool_timeout: env_f64("LLM_TOOL_TIMEOUT", 60.0),
            max_tool_chars: env_usize("LLM_MAX_TOOL_CHARS", 16000),
            max_history: env_usize("LLM_MAX_HISTORY", 40).max(1),
        }
    }
}

/// Python type(e).__name__ 的替身:让 [错误] 文案的形态保持一致。
#[derive(Debug, Clone)]
pub enum ErrKind {
    Json,
    Value,
    Lookup,
    Type,
    Os,
}

impl ErrKind {
    pub fn name(&self) -> &'static str {
        match self {
            ErrKind::Json => "JSONDecodeError",
            ErrKind::Value => "ValueError",
            ErrKind::Lookup => "LookupError",
            ErrKind::Type => "TypeError",
            ErrKind::Os => "OSError",
        }
    }
}

#[derive(Debug)]
pub struct ToolError {
    pub kind: ErrKind,
    pub msg: String,
}

impl ToolError {
    fn new(kind: ErrKind, msg: impl Into<String>) -> Self {
        ToolError { kind, msg: msg.into() }
    }
}

fn obj(pairs: Vec<(&str, Json)>) -> Json {
    Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn s(v: &str) -> Json {
    Json::Str(v.to_string())
}

fn p(t: &str, d: &str) -> Json {
    obj(vec![("type", s(t)), ("description", s(d))])
}

fn spec(name: &str, desc: &str, props: Vec<(&str, Json)>, req: &[&str]) -> Json {
    obj(vec![
        ("type", s("function")),
        (
            "function",
            obj(vec![
                ("name", s(name)),
                ("description", s(desc)),
                (
                    "parameters",
                    obj(vec![
                        ("type", s("object")),
                        ("properties", obj(props)),
                        ("required", Json::Arr(req.iter().map(|r| s(r)).collect())),
                        ("additionalProperties", Json::Bool(false)),
                    ]),
                ),
            ]),
        ),
    ])
}

pub fn tool_schemas() -> Vec<Json> {
    vec![
        spec("tool_bash", "执行系统命令", vec![("cmd", p("string", "命令"))], &["cmd"]),
        spec("tool_read", "读取文件", vec![("fp", p("string", "文件路径"))], &["fp"]),
        spec(
            "tool_write",
            "写入文件",
            vec![("fp", p("string", "文件路径")), ("data", p("string", "内容"))],
            &["fp", "data"],
        ),
    ]
}

fn bash_program() -> String {
    if Path::new("/bin/bash").exists() {
        "/bin/bash".into()
    } else {
        "sh".into()
    }
}

const TIMEOUT_MARK: &str = "__timeout__";

/// (stdout, stderr, 退出码)
type Captured = (Vec<u8>, Vec<u8>, Option<i32>);

/// 对标 Python 的 {x:g}:整数不带小数点,小数按最短表示。
fn fmt_num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{}", x)
    }
}

/// 带超时执行子进程。std 没有现成超时:用 try_wait + 轮询,超时后 kill。
/// stdout/stderr 由独立线程读取,避免子进程写满管道后死锁。
fn run_captured(cmd: &mut Command, timeout: f64) -> Result<Captured, ToolError> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::new(ErrKind::Os, e.to_string()))?;
    let mut so = child.stdout.take().expect("stdout");
    let mut se = child.stderr.take().expect("stderr");
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let deadline = Instant::now() + Duration::from_secs_f64(timeout.max(0.0));
    let mut status: Option<ExitStatus> = None;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                status = Some(st);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(ToolError::new(ErrKind::Os, e.to_string())),
        }
    }
    let out = t_out.join().unwrap_or_default();
    let err = t_err.join().unwrap_or_default();
    if timed_out {
        return Err(ToolError::new(ErrKind::Os, TIMEOUT_MARK));
    }
    Ok((out, err, status.and_then(|s| s.code())))
}

pub fn tool_bash(cmd: &str, timeout: f64) -> Result<String, ToolError> {
    let mut c = Command::new(bash_program());
    c.arg("-c").arg(cmd);
    match run_captured(&mut c, timeout) {
        Ok((out, err, code)) => {
            let mut s = String::from_utf8_lossy(&out).into_owned();
            let err = String::from_utf8_lossy(&err).into_owned();
            if !err.is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            if let Some(code) = code {
                if code != 0 {
                    s.push_str(&format!("\n[exit:{}]", code));
                }
            }
            let s = s.trim().to_string();
            Ok(if s.is_empty() { "(无输出)".into() } else { s })
        }
        // 与 Python 版一致:超时是工具自己返回的文案,不再套一层“调用失败”。
        Err(e) if e.msg == TIMEOUT_MARK => Ok(format!("[错误]命令超时(>{}s)", fmt_num(timeout))),
        Err(e) => Err(e),
    }
}

pub fn tool_read(fp: &str) -> Result<String, ToolError> {
    // 按 UTF-8 读取,非法字节替换,与 Python errors="replace" 对齐。
    let bytes = fs::read(fp).map_err(|e| ToolError::new(ErrKind::Os, e.to_string()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn tool_write(fp: &str, data: &str) -> Result<String, ToolError> {
    if let Some(dir) = PathBuf::from(fp).parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).map_err(|e| ToolError::new(ErrKind::Os, e.to_string()))?;
        }
    }
    fs::write(fp, data).map_err(|e| ToolError::new(ErrKind::Os, e.to_string()))?;
    Ok(format!("OK—写入{}字符", data.chars().count()))
}

fn req_str(args: &Json, k: &str) -> Result<String, ToolError> {
    match args.get(k) {
        Some(Json::Str(v)) => Ok(v.clone()),
        Some(v) => Err(ToolError::new(
            ErrKind::Type,
            format!("{}() argument must be str, not {}", k, v.py_type_name()),
        )),
        None => Err(ToolError::new(
            ErrKind::Type,
            format!("{}() missing 1 required argument", k),
        )),
    }
}

/// 按名字分派(像 Python 的 **kwargs 一样按参数名取值,不依赖顺序)。
pub fn call_tool(name: &str, args: &Json, timeout: f64) -> Result<String, ToolError> {
    match name {
        "tool_bash" => tool_bash(&req_str(args, "cmd")?, timeout),
        "tool_read" => tool_read(&req_str(args, "fp")?),
        "tool_write" => tool_write(&req_str(args, "fp")?, &req_str(args, "data")?),
        _ => Err(ToolError::new(ErrKind::Lookup, format!("未知工具:{}", name))),
    }
}

/// 执行单个 tool_call:任何异常都转成一条 [错误] 结果,保证 tool_call 有配对响应。
pub fn run_tool(tc: &Json, cfg: &Config) -> String {
    let func = tc.get("function");
    let name = func
        .and_then(|f| f.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let out = (|| -> Result<String, ToolError> {
        let raw = match func.and_then(|f| f.get("arguments")) {
            Some(Json::Str(v)) if !v.trim().is_empty() => v.clone(),
            _ => "{}".to_string(),
        };
        let args = json::parse(&raw)
            .map_err(|e| ToolError::new(ErrKind::Json, format!("Expecting value ({})", e)))?;
        if !matches!(args, Json::Obj(_)) {
            return Err(ToolError::new(
                ErrKind::Value,
                format!("arguments 必须是 JSON 对象,收到 {}", args.py_type_name()),
            ));
        }
        call_tool(&name, &args, cfg.tool_timeout)
    })();
    let out = match out {
        Ok(v) => v,
        Err(e) => format!(
            "[错误]工具 {} 调用失败:{}: {}",
            if name.is_empty() { "?" } else { &name },
            e.kind.name(),
            e.msg
        ),
    };
    let n = out.chars().count();
    if n <= cfg.max_tool_chars {
        out
    } else {
        let head: String = out.chars().take(cfg.max_tool_chars).collect();
        format!("{}\n…[已截断,原始 {} 字符]", head, n)
    }
}

fn get_str<'a>(m: &'a Json, k: &str) -> Option<&'a str> {
    m.get(k).and_then(|v| v.as_str())
}

/// 裁剪过旧历史,但绝不把 tool 响应与它的 tool_calls 拆开(否则后续请求 400)。
pub fn trim(ctx: &[Json], max_history: usize) -> Vec<Json> {
    if ctx.len() <= max_history + 1 {
        return ctx.to_vec();
    }
    let mut cut = ctx.len() - max_history;
    while cut < ctx.len() && get_str(&ctx[cut], "role") == Some("tool") {
        cut += 1;
    }
    let mut out = Vec::with_capacity(ctx.len() - cut + 1);
    out.push(ctx[0].clone());
    out.extend_from_slice(&ctx[cut..]);
    out
}

/// POST {BASE_URL}/chat/completions,返回 choices[0].message。
/// HTTPS 由系统 curl 完成(标准库无 TLS);-w 取状态码,非 2xx 也能拿到响应体。
pub fn chat(cfg: &Config, messages: &[Json]) -> Result<Json, String> {
    let body = obj(vec![
        ("model", s(&cfg.model)),
        ("messages", Json::Arr(messages.to_vec())),
        ("tools", Json::Arr(tool_schemas())),
    ])
    .to_string();
    let url = format!("{}/chat/completions", cfg.base_url);
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            &cfg.request_timeout.to_string(),
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json; charset=utf-8",
            "-H",
            &format!("Authorization: Bearer {}", cfg.api_key),
            "-H",
            "User-Agent: mvp-agent/1.0",
            // 关掉 Expect: 100-continue,避免大请求体时白等一秒。
            "-H",
            "Expect:",
            "-w",
            "\n%{http_code}",
            "--data-binary",
            "@-",
            &url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("网络错误:无法启动 curl:{}", e))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(body.as_bytes());
    }
    let (out, err, code) = read_child(&mut child).map_err(|e| format!("网络错误:{}", e))?;
    let stderr = String::from_utf8_lossy(&err);
    if code != Some(0) {
        let detail = if stderr.trim().is_empty() {
            format!("curl 退出码 {:?}", code)
        } else {
            stderr.trim().to_string()
        };
        return Err(format!("网络错误:{}", detail));
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    // -w 把状态码追加在最后一行;响应体自身可能含换行。
    let idx = text.rfind('\n').ok_or("网络错误:curl 无响应")?;
    let (body_txt, status_txt) = (&text[..idx], text[idx + 1..].trim());
    let status: u32 = status_txt
        .parse()
        .map_err(|_| format!("网络错误:无法解析状态码 {:?}", status_txt))?;
    if !(200..300).contains(&status) {
        let snippet: String = body_txt.chars().take(1000).collect();
        return Err(format!("HTTP {} {}: {}", status, http_reason(status), snippet));
    }
    let v = json::parse(body_txt).map_err(|e| format!("网络错误:响应不是合法 JSON:{}", e))?;
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .cloned()
        .ok_or_else(|| "网络错误:响应缺少 choices[0].message".to_string())
}

fn read_child(child: &mut Child) -> Result<Captured, String> {
    let mut so = child.stdout.take().ok_or("无 stdout")?;
    let mut se = child.stderr.take().ok_or("无 stderr")?;
    let t = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let mut out = Vec::new();
    so.read_to_end(&mut out).map_err(|e| e.to_string())?;
    let err = t.join().unwrap_or_default();
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok((out, err, status.code()))
}

fn http_reason(code: u32) -> &'static str {
    match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// 主循环:追加 user,最多 MAX_TURNS 轮;**每次请求前**裁剪历史。
pub fn agent_loop(cfg: &Config, inp: &str, ctx: &mut Vec<Json>) -> String {
    ctx.push(obj(vec![("role", s("user")), ("content", s(inp))]));
    for _ in 0..cfg.max_turns {
        // 安全边界:此刻上一轮 tool_calls 都已有配对响应。
        *ctx = trim(ctx, cfg.max_history);
        let m = match chat(cfg, ctx) {
            Ok(m) => m,
            Err(e) => {
                let low = e.to_lowercase();
                let kind = if low.contains("context") || low.contains("token") {
                    "上下文过长,请重开会话"
                } else {
                    "API失败"
                };
                return format!("[错误]{}:{}", kind, e);
            }
        };
        let mut a: Vec<(String, Json)> = Vec::new();
        for k in ["role", "content", "tool_calls"] {
            if let Some(v) = m.get(k) {
                a.push((k.to_string(), v.clone()));
            }
        }
        ctx.push(Json::Obj(a));
        let calls = match m.get("tool_calls") {
            Some(Json::Arr(a)) if !a.is_empty() => a.clone(),
            _ => {
                return m.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            }
        };
        for tc in &calls {
            let f = tc.get("function");
            eprintln!(
                "[tool_call]{}({})",
                f.and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("null"),
                f.and_then(|f| f.get("arguments")).and_then(|v| v.as_str()).unwrap_or("null")
            );
            ctx.push(obj(vec![
                ("role", s("tool")),
                ("tool_call_id", tc.get("id").cloned().unwrap_or(Json::Null)),
                ("content", Json::Str(run_tool(tc, cfg))),
            ]));
        }
    }
    format!("[Agent]防止死循环,达到最大轮次({}),中断", cfg.max_turns)
}

fn main() {
    let cfg = Config::from_env();
    if cfg.api_key.is_empty() {
        eprintln!("错误:请设置LLM_API_KEY");
        std::process::exit(1);
    }
    let mut ctx: Vec<Json> = vec![obj(vec![("role", s("system")), ("content", s(SYS_PROMPT))])];
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() {
        // 非交互:cargo run --quiet -- "任务"
        println!("助手>{}", agent_loop(&cfg, &args.join(" "), &mut ctx));
        return;
    }
    eprintln!("智能助手 {} 就绪。输入消息(或'exit'退出)。\n", cfg.model);
    loop {
        print!("用户> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!("\n再见。");
                break;
            }
            Ok(_) => {}
        }
        let inp = line.trim().to_string();
        if inp.is_empty() {
            continue;
        }
        if inp.eq_ignore_ascii_case("exit") || inp.eq_ignore_ascii_case("quit") {
            println!("再见。");
            break;
        }
        eprintln!("处理请求:{}", inp);
        println!("\n助手>{}\n", agent_loop(&cfg, &inp, &mut ctx));
    }
}
