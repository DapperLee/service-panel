use crate::types::LogLine;
use serde::Serialize;
use std::collections::HashMap;
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

/// 推日志给前端的闭包（实际实现是往 LogQueue 里入队）
pub type LogEmitter = Arc<dyn Fn(LogEvent) + Send + Sync>;

/// 一个服务的日志写入端：**在写路径上**按「跨天」与「累计字节」两个维度滚动。
///
/// 每个服务全局只持有这一个实例（缓存在 Runtime.sink 里），所以同一时刻只有一个
/// 句柄开着某个日志文件。这一点很关键：旧实现每次写系统消息都要重新 open 一个文件，
/// 多句柄并存时既没法安全重命名（Windows 上会因共享冲突失败），也没法累计出
/// 到底写了多少字节——20MB 上限因此形同虚设。
pub struct LogSink {
    dir: PathBuf,
    day: String,
    /// 当天分片序号：0 = `{day}.log`，1..N = `{day}.log.N`
    shard: u32,
    file: File,
    /// 当前分片已写字节数（含接手续写时文件的既有大小）
    written: u64,
    /// 单个分片的字节上限
    max_bytes: u64,
}

/// 共享句柄：stdout/stderr 读取线程、文件尾随线程、系统消息都往这一个里写
pub type Sink = Arc<Mutex<LogSink>>;

fn day_now() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn path_for(dir: &Path, day: &str, shard: u32) -> PathBuf {
    if shard == 0 {
        dir.join(format!("{day}.log"))
    } else {
        dir.join(format!("{day}.log.{shard}"))
    }
}

impl LogSink {
    /// 打开（或接续）该服务的日志写入端。面板重启后应继续写当天最后一个分片，
    /// 而不是悄悄另起一个新文件。
    pub fn open(dir: &Path) -> std::io::Result<Sink> {
        Self::open_with_limit(dir, MAX_LOG_BYTES)
    }

    /// `open` 的实现，分片阈值可注入（单测用小阈值验证"写路径上真的会滚"）。
    pub fn open_with_limit(dir: &Path, max_bytes: u64) -> std::io::Result<Sink> {
        std::fs::create_dir_all(dir)?;
        let day = day_now();
        let mut shard = 0u32;
        while shard < 999 && path_for(dir, &day, shard + 1).exists() {
            shard += 1;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path_for(dir, &day, shard))?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Arc::new(Mutex::new(Self {
            dir: dir.to_path_buf(),
            day,
            shard,
            file,
            written,
            max_bytes,
        })))
    }

    /// 跨天 -> 换到新一天的基础名；超上限 -> 当天下一个分片。
    /// 只新增文件、不重命名旧文件：没有共享冲突，也没有"重命名失败"的丢日志分支。
    fn roll_if_needed(&mut self) {
        let today = day_now();
        let new_day = today != self.day;
        let oversize = self.written >= self.max_bytes;
        if !new_day && !oversize {
            return;
        }
        let day = if new_day { today } else { self.day.clone() };
        let mut shard = if new_day { 0 } else { self.shard + 1 };
        while path_for(&self.dir, &day, shard).exists() {
            shard += 1;
        }
        if let Ok(f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path_for(&self.dir, &day, shard))
        {
            self.file = f;
            self.day = day;
            self.shard = shard;
            self.written = 0;
        }
        // 打不开新分片就继续写当前文件：宁可不滚动，也不能丢日志
    }

    fn write_line(&mut self, ts: i64, stream: &str, text: &str) {
        self.roll_if_needed();
        let payload = format!("{ts} [{stream}] {text}\n");
        if self.file.write_all(payload.as_bytes()).is_ok() {
            let _ = self.file.flush();
            self.written += payload.len() as u64;
        }
    }
}

/// 删除 keep_days 之前修改过的日志文件（含 .log 与 .log.N），控制磁盘占用。
/// 递归遍历：服务日志在 `logs/<id>/`，任务日志在 `logs/_tasks/<run_id>/`，都要覆盖。
/// 删空后的运行目录一并移除，避免 `_tasks` 下堆积空壳。
pub fn cleanup_old_logs(log_root: &Path, keep_days: i64) {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(keep_days.max(1) as u64 * 86400);
    cleanup_dir(log_root, cutoff, false);
}

fn cleanup_dir(dir: &Path, cutoff: std::time::SystemTime, is_root: bool) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // 递归进子目录（服务 id / _tasks / _tasks/<run_id>）
            cleanup_dir(&p, cutoff, false);
            // 子目录空了且非根，删掉（刚建还没写的当前目录也可能会被判空，
            // 但保留根目录，且只删完全空的目录，风险可控）
            if let Ok(mut it) = std::fs::read_dir(&p) {
                if it.next().is_none() {
                    let _ = std::fs::remove_dir(&p);
                }
            }
            continue;
        }
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
    let _ = is_root;
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
pub fn emit_line(emitter: &LogEmitter, sink: &Sink, service_id: &str, stream: &str, text: &str) {
    emit_and_persist(text, stream, service_id, emitter, sink);
}

fn emit_and_persist(text: &str, stream: &str, service_id: &str, emitter: &LogEmitter, sink: &Sink) {
    let line = LogLine {
        ts: now_millis(),
        stream: stream.to_string(),
        text: text.to_string(),
    };
    // 落盘（滚动在 LogSink 内部按累计字节判定）
    if let Ok(mut f) = sink.lock() {
        f.write_line(line.ts, &line.stream, &line.text);
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
    emitter: LogEmitter,
    sink: Sink,
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
pub fn system_message(text: &str, service_id: &str, emitter: &LogEmitter, sink: &Sink) {
    emit_and_persist(text, "system", service_id, emitter, sink);
}

/// 有界日志队列：emitter 只负责入队，flusher 线程周期性取走一批再 emit 给前端，
/// 避免话痨服务每秒上千次 IPC。
///
/// 队列满时**丢弃新行并按服务计数**——落盘发生在入队之前，所以丢掉的只是 UI 流，
/// 磁盘上的日志仍然完整。取走时给发生过丢弃的服务补一条系统提示，让用户知道有跳跃。
pub struct LogQueue {
    items: Vec<LogEvent>,
    dropped: HashMap<String, usize>,
    capacity: usize,
}

impl LogQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: Vec::new(),
            dropped: HashMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// 入队。返回 false 表示队列已满、该行被丢弃。
    pub fn push(&mut self, ev: LogEvent) -> bool {
        if self.items.len() >= self.capacity {
            *self.dropped.entry(ev.service_id).or_insert(0) += 1;
            return false;
        }
        self.items.push(ev);
        true
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.dropped.is_empty()
    }

    /// 取走本批（按服务附上「已丢弃 N 行」提示），并清空队列。
    pub fn take_batch(&mut self) -> Vec<LogEvent> {
        let mut out = std::mem::take(&mut self.items);
        for (service_id, n) in self.dropped.drain() {
            out.push(LogEvent {
                service_id,
                line: LogLine {
                    ts: now_millis(),
                    stream: "system".to_string(),
                    text: format!("[panel] 输出过快，已丢弃 {n} 行（磁盘日志完整）"),
                },
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, LogEvent, LogQueue, LogSink};
    use crate::types::LogLine;
    use std::path::PathBuf;

    fn line(svc: &str, text: &str) -> LogEvent {
        LogEvent {
            service_id: svc.to_string(),
            line: LogLine {
                ts: 0,
                stream: "stdout".to_string(),
                text: text.to_string(),
            },
        }
    }

    /// 唯一的临时日志目录（面板自己生成的临时文件，测完删掉）
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("service-panel-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    /// 核心诉求：超过上限要在**写路径上**滚动。旧实现只在 LogFile::create（即每次
    /// 服务启动）判一次，跑一周不停的服务照样能写出几 GB 的单文件。
    #[test]
    fn sink_rolls_on_write_path() {
        let dir = temp_dir("roll");
        let sink = LogSink::open_with_limit(&dir, 400).unwrap();
        for _ in 0..40 {
            if let Ok(mut s) = sink.lock() {
                s.write_line(0, "stdout", &"x".repeat(36));
            }
        }
        assert!(dir.join(format!("{}.log", today())).exists());
        assert!(
            dir.join(format!("{}.log.1", today())).exists(),
            "超过阈值后应在写入过程中开新分片，而不是继续往同一个文件堆"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重启后面板应续写当天最后一个分片，而不是悄悄另起一个新文件。
    #[test]
    fn sink_reopens_latest_shard() {
        let dir = temp_dir("reopen");
        {
            let sink = LogSink::open(&dir).unwrap();
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_line(0, "stdout", "first");
        }
        {
            let sink = LogSink::open(&dir).unwrap();
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .write_line(0, "stdout", "second");
        }
        let content = std::fs::read_to_string(dir.join(format!("{}.log", today()))).unwrap();
        assert!(
            content.contains("first") && content.contains("second"),
            "两次打开应写进同一个文件，实际内容: {content}"
        );
        assert!(
            !dir.join(format!("{}.log.1", today())).exists(),
            "不该另起新分片"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn queue_keeps_all_when_not_full() {
        let mut q = LogQueue::new(10);
        assert!(q.push(line("a", "1")));
        assert!(q.push(line("a", "2")));
        let batch = q.take_batch();
        assert_eq!(batch.len(), 2);
        // 没有丢弃就不该有提示行
        assert!(batch.iter().all(|e| e.line.stream == "stdout"));
        assert!(q.is_empty());
    }

    #[test]
    fn queue_drops_over_capacity_and_marks_per_service() {
        let mut q = LogQueue::new(3);
        for i in 0..5 {
            q.push(line("a", &i.to_string()));
        }
        for i in 0..2 {
            q.push(line("b", &i.to_string()));
        }
        let batch = q.take_batch();
        // 3 条保留 + 2 个服务各一条丢弃提示（a 丢 2 行、b 丢 2 行）
        let sys: Vec<&LogEvent> = batch.iter().filter(|e| e.line.stream == "system").collect();
        assert_eq!(sys.len(), 2);
        assert_eq!(
            batch.iter().filter(|e| e.line.stream == "stdout").count(),
            3
        );
        for e in &sys {
            assert!(
                e.line.text.contains("2 行"),
                "提示应带丢弃条数: {}",
                e.line.text
            );
        }
        assert!(q.is_empty());
    }

    #[test]
    fn queue_capacity_at_least_one() {
        // capacity 传 0 也不能把队列变成「全丢弃」
        let mut q = LogQueue::new(0);
        assert!(q.push(line("a", "1")));
        assert_eq!(q.take_batch().len(), 1);
    }
}
