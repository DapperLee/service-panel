use crate::types::{HealthCheck, HealthKind};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 执行一次健康检查，返回 Ok(()) 表示健康。
/// 所有分支都有超时上限，绝不无限阻塞（否则会在监督线程里拖死整轮 tick）。
pub fn check(hc: &HealthCheck) -> Result<(), String> {
    let timeout = Duration::from_secs(hc.timeout_secs.max(1));
    match hc.kind {
        HealthKind::None => Ok(()),
        HealthKind::Tcp => {
            let addr = hc
                .target
                .parse::<std::net::SocketAddr>()
                .or_else(|_| resolve(&hc.target))?;
            TcpStream::connect_timeout(&addr, timeout)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        HealthKind::Http => http_probe(&hc.target, timeout),
        HealthKind::Command => command_probe(&hc.target, timeout),
    }
}

fn resolve(s: &str) -> Result<std::net::SocketAddr, String> {
    let addrs: Vec<_> = match std::net::ToSocketAddrs::to_socket_addrs(s) {
        Ok(it) => it.collect(),
        Err(e) => return Err(e.to_string()),
    };
    addrs.into_iter().next().ok_or("无法解析地址".to_string())
}

/// 极简 HTTP GET：只看状态码；仅支持 http://（localhost 场景够用）。
/// 连接、写、读全部带超时——否则一个「接受连接但永不回包」的服务会永久卡住探测。
fn http_probe(url: &str, timeout: Duration) -> Result<(), String> {
    let rest = url.strip_prefix("http://").ok_or("仅支持 http://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let socket_addr: std::net::SocketAddr = authority.parse().or_else(|_| resolve(authority))?;
    let mut stream = TcpStream::connect_timeout(&socket_addr, timeout).map_err(|e| e.to_string())?;
    // 关键：读写都设超时，防止连上后对端不发数据导致 read_line 永久阻塞
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_read_timeout(Some(timeout));
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: service-panel\r\n\r\n",
        path, authority
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(&stream);
    let mut status_line = String::new();
    // 读超时/无换行都会返回 Err，不会卡死
    let n = reader.read_line(&mut status_line).map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("空响应".to_string());
    }
    let code = parse_status_line(&status_line)?;
    if (200..400).contains(&code) {
        Ok(())
    } else {
        Err(format!("HTTP {code}"))
    }
}

/// 从 "HTTP/1.1 200 OK" 抽状态码（纯函数，便于单测）
fn parse_status_line(line: &str) -> Result<u16, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    parts
        .get(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| "无法解析状态码".to_string())
}

/// 命令型探测：spawn 后轮询等待，超时则强杀，避免挂死的命令拖住监督线程。
fn command_probe(target: &str, timeout: Duration) -> Result<(), String> {
    let (prog, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };
    let mut child = Command::new(prog)
        .args([flag, target])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("exit={:?}", status.code()))
                }
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("命令超时 {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(e.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_status_line;

    #[test]
    fn parse_status_ok() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK").unwrap(), 200);
        assert_eq!(parse_status_line("HTTP/1.0 302 Found").unwrap(), 302);
        assert_eq!(parse_status_line("HTTP/1.1 404 Not Found").unwrap(), 404);
    }

    #[test]
    fn parse_status_bad() {
        assert!(parse_status_line("garbage").is_err());
        assert!(parse_status_line("HTTP/1.1 abc").is_err());
        assert!(parse_status_line("").is_err());
    }
}
