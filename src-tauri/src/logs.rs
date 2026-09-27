use crate::types::LogLine;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 单个日志文件的大小上限（字节），超过则滚动到 .1/.2…
const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;

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
        let base = log_dir.join(format!("{}.log", day));
        // 大小维度滚动：今天的文件超过上限就改名成 .1/.2…，另起新文件
        let path = rotate_if_oversized(&base);
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn writer(&self) -> Arc<Mutex<File>> {
        self.file.clone()
    }
}

/// 若 base 文件超过大小上限，滚动为 base.1 / base.2 …（最多保留若干份，旧的由清理删除）
fn rotate_if_oversized(base: &Path) -> PathBuf {
    let oversized = std::fs::metadata(base).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false);
    if !oversized {
        return base.to_path_buf();
    }
    // 找一个未占用的 .N 后缀
    for n in 1..=50u32 {
        let rolled = base.with_extension(format!("log.{n}"));
        if !rolled.exists() {
            let _ = std::fs::rename(base, &rolled);
            break;
        }
    }
    base.to_path_buf()
}

/// 删除 keep_days 之前修改过的日志文件（含 .log 与 .log.N），控制磁盘占用。
pub fn cleanup_old_logs(log_root: &Path, keep_days: i64) {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(keep_days.max(1) as u64 * 86400);
    let rd = match std::fs::read_dir(log_root) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for svc in rd.flatten() {
        let files = match std::fs::read_dir(svc.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for entry in files.flatten() {
            let p = entry.path();
            let is_log = p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains(".log"))
                .unwrap_or(false);
            if !is_log {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    if mtime < cutoff {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn decode_utf8() {
        assert_eq!(decode("中文abc".as_bytes(), "utf-8"), "中文abc");
    }

    #[test]
    fn decode_gbk() {
        // "中文" 的 GBK 编码字节
        let gbk = [0xD6u8, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode(&gbk, "gbk"), "中文");
    }

    #[test]
    fn decode_auto_prefers_utf8() {
        let utf8 = "中文".as_bytes();
        assert_eq!(decode(utf8, "auto"), "中文");
    }

    #[test]
    fn decode_auto_falls_back_to_gbk() {
        // GBK 字节对严格 UTF-8 非法，auto 应回退 GBK
        let gbk = [0xD6u8, 0xD0, 0xCE, 0xC4];
        assert_eq!(decode(&gbk, "auto"), "中文");
    }
}
