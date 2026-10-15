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
        HealthKind::Http => http_probe(&hc.target, timeout, hc.tls_insecure),
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

/// http/https 探测目标（纯解析结果，便于单测）
#[derive(Debug, PartialEq)]
pub struct HttpTarget {
    /// 主机名或 IP 字面量，不含端口与方括号
    pub host: String,
    pub port: u16,
    /// 请求路径（含 query），缺省 "/"
    pub path: String,
    pub tls: bool,
}

/// 拆 "scheme://host[:port]/path"。缺省端口按方案取 80 / 443。
pub fn parse_http_target(url: &str) -> Result<HttpTarget, String> {
    let (rest, default_port, tls) = if let Some(r) = url.strip_prefix("https://") {
        (r, 443u16, true)
    } else if let Some(r) = url.strip_prefix("http://") {
        (r, 80u16, false)
    } else {
        return Err("健康检查 URL 必须以 http:// 或 https:// 开头".to_string());
    };
    let (authority, path) = match rest.find('/').or(rest.find('?')) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = split_host_port(authority, default_port);
    if host.is_empty() {
        return Err("URL 缺少主机名".to_string());
    }
    Ok(HttpTarget {
        host: host.to_string(),
        port,
        path: path.to_string(),
        tls,
    })
}

fn split_host_port(authority: &str, default_port: u16) -> (&str, u16) {
    // IPv6 字面量形如 [::1]:8443，冒号出现在方括号内不算端口分隔符
    if let Some(idx) = authority.rfind(']') {
        let host = &authority[..=idx];
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let port = authority[idx + 1..]
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(default_port);
        return (bare, port);
    }
    if let Some((h, p)) = authority.rsplit_once(':') {
        if !p.is_empty() {
            if let Ok(port) = p.parse::<u16>() {
                return (h, port);
            }
        }
    }
    (authority, default_port)
}

/// 主机名 -> SocketAddr。IP 字面量直接拼端口，避免走一遍 DNS。
fn resolve_host(host: &str, port: u16) -> Result<std::net::SocketAddr, String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(ip, port));
    }
    resolve(&format!("{host}:{port}"))
}

/// 读写都设超时，防止连上后对端不发数据导致 read_line 永久阻塞。
trait ReadWrite: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> ReadWrite for T {}

/// HTTP/HTTPS GET：只看状态码。连接、握手、写、读全部带超时。
/// https 走系统 TLS（Windows 上是 Schannel），默认校验证书；
/// `tls_insecure` 打开时放过自签证书与域名不匹配——本地 dev 证书场景。
fn http_probe(url: &str, timeout: Duration, tls_insecure: bool) -> Result<(), String> {
    let t = parse_http_target(url)?;
    let addr = resolve_host(&t.host, t.port)?;
    let stream = TcpStream::connect_timeout(&addr, timeout).map_err(|e| e.to_string())?;
    // 关键：读写都设超时，防止连上后对端不发数据导致 read_line 永久阻塞
    let _ = stream.set_write_timeout(Some(timeout));
    let _ = stream.set_read_timeout(Some(timeout));

    let mut rw: Box<dyn ReadWrite> = if t.tls {
        let mut builder = native_tls::TlsConnector::builder();
        if tls_insecure {
            builder.danger_accept_invalid_certs(true);
            builder.danger_accept_invalid_hostnames(true);
        }
        let connector = builder.build().map_err(|e| e.to_string())?;
        // SNI/校验用主机名（不是解析后的 IP），所以这里传 t.host
        Box::new(
            connector
                .connect(t.host.as_str(), stream)
                .map_err(|e| format!("TLS 握手失败: {e}"))?,
        )
    } else {
        Box::new(stream)
    };

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: service-panel\r\n\r\n",
        t.path, t.host
    );
    rw.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(&mut rw);
    let mut status_line = String::new();
    // 读超时/无换行都会返回 Err，不会卡死
    let n = reader
        .read_line(&mut status_line)
        .map_err(|e| e.to_string())?;
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
    let (prog, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let mut cmd = Command::new(prog);
    cmd.args([flag, target])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::drivers::command::hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
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
    use super::{parse_http_target, parse_status_line, HttpTarget};

    #[test]
    fn parse_http_defaults_and_tls() {
        assert_eq!(
            parse_http_target("http://localhost/health"),
            Ok(HttpTarget {
                host: "localhost".to_string(),
                port: 80,
                path: "/health".to_string(),
                tls: false,
            })
        );
        assert_eq!(
            parse_http_target("https://api.example.com"),
            Ok(HttpTarget {
                host: "api.example.com".to_string(),
                port: 443,
                path: "/".to_string(),
                tls: true,
            })
        );
    }

    #[test]
    fn parse_http_explicit_port_and_query() {
        let t = parse_http_target("https://127.0.0.1:8443/api/ping?x=1").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str(), t.tls),
            ("127.0.0.1", 8443, "/api/ping?x=1", true)
        );
    }

    #[test]
    fn parse_http_ipv6_literal_keeps_bracket_colons_out_of_port() {
        let t = parse_http_target("http://[::1]:9000/p").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 9000));
        // 无端口的 IPv6 不能被末尾的 "] " 误当成端口分隔
        let t2 = parse_http_target("https://[fe80::1]").unwrap();
        assert_eq!((t2.host.as_str(), t2.port, t2.tls), ("fe80::1", 443, true));
    }

    #[test]
    fn parse_http_rejects_missing_scheme() {
        assert!(parse_http_target("localhost:8080/health").is_err());
        assert!(parse_http_target("ftp://host/").is_err());
        assert!(parse_http_target("https://").is_err());
    }

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
