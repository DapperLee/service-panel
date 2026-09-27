use crate::types::LogLine;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 广播给前端的日志事件负载
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub service_id: String,
    pub line: LogLine,
}

/// 一个服务今天的日志文件（按天滚动），stdout/stderr/系统消息都写进来
pub struct LogFile {
    file: Arc<Mutex<File>>,
}

impl LogFile {
    pub fn create(log_dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(log_dir)?;
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join(format!("{}.log", day)))?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn writer(&self) -> Arc<Mutex<File>> {
        self.file.clone()
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn decode(bytes: &[u8], encoding: &str) -> String {
    match encoding.to_ascii_lowercase().as_str() {
        "gbk" => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
        // 自动：优先按 UTF-8 严格解码，遇到非法字节再回退 GBK。
        // 换行符 0x0A 既不是 UTF-8 多字节序列的一部分，也不在 GBK 尾字节范围内，
        // 所以按整行字节判定是安全的。
        "auto" => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                let (cow, _, _) = encoding_rs::GBK.decode(bytes);
                cow.into_owned()
            }
        },
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// 供外部（文件尾随等）复用的解码
pub fn decode_text(bytes: &[u8], encoding: &str) -> String {
    decode(bytes, encoding)
}

/// 供外部复用：发一行日志到 UI + 落盘
pub fn emit_line(
    emitter: &Arc<dyn Fn(LogEvent) + Send + Sync>,
    sink: &Arc<Mutex<File>>,
    service_id: &str,
    stream: &str,
    text: &str,
) {
    emit_and_persist(text, stream, service_id, emitter, sink);
}

fn emit_and_persist(
    text: &str,
    stream: &str,
    service_id: &str,
    emitter: &Arc<dyn Fn(LogEvent) + Send + Sync>,
    sink: &Arc<Mutex<File>>,
) {
    let line = LogLine {
        ts: now_millis(),
        stream: stream.to_string(),
        text: text.to_string(),
    };
    // 落盘
    if let Ok(mut f) = sink.lock() {
        let _ = writeln!(f, "{} [{}] {}", line.ts, line.stream, line.text);
        let _ = f.flush();
    }
    // 推给前端
    emitter(LogEvent {
        service_id: service_id.to_string(),
        line,
    });
}

/// 后台线程持续读子进程管道，逐行解码 -> 落盘 + 广播。
/// 返回 JoinHandle（调用方可丢弃，线程随进程结束自然退出）。
pub fn spawn_reader<T: Read + Send + 'static>(
    stream: T,
    encoding: String,
    name: &'static str,
    service_id: String,
    emitter: Arc<dyn Fn(LogEvent) + Send + Sync>,
    sink: Arc<Mutex<File>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break, // EOF：进程结束
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                        buf.pop();
                    }
                    let text = decode(&buf, &encoding);
                    emit_and_persist(&text, name, &service_id, &emitter, &sink);
                }
                Err(_) => break,
            }
        }
    })
}

/// 写一条系统消息（启动/停止/崩溃等）到日志与前端
pub fn system_message(
    text: &str,
    service_id: &str,
    emitter: &Arc<dyn Fn(LogEvent) + Send + Sync>,
    sink: &Arc<Mutex<File>>,
) {
    emit_and_persist(text, "system", service_id, emitter, sink);
}
