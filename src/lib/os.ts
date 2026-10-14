// 系统服务驱动在前后端的固定标识是 windows_service（Rust 内部按平台分派
// sc / systemctl / launchctl）。它会写进 panel.db，改名会导致老配置失效，
// 所以只在展示文案上按平台区分。
export type OsName = "windows" | "macos" | "linux";

function detectOs(): OsName {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  if (/macintosh|mac os x|iphone|ipad/i.test(ua)) return "macos";
  if (/linux|x11/i.test(ua)) return "linux";
  return "windows";
}

export const OS: OsName = detectOs();

export const PLATFORM = {
  card: OS === "windows" ? "Windows 服务" : "系统服务",
  option:
    OS === "windows"
      ? "Windows 服务"
      : OS === "linux"
        ? "系统服务（systemd 单元）"
        : "系统服务（launchd 标签）",
  nameLabel:
    OS === "windows"
      ? "Windows 服务名（sc query 用的名字）"
      : OS === "linux"
        ? "systemd 单元名（systemctl 用，可省略 .service）"
        : "launchd 标签（plist 的 Label，launchctl list 可见）",
  namePlaceholder:
    OS === "windows"
      ? "MySQL80 / Redis"
      : OS === "linux"
        ? "redis / nginx"
        : "homebrew.mxcl.redis（brew services 装的就是这个格式）",
  nameRequired:
    OS === "windows"
      ? "请填写 Windows 服务名"
      : OS === "linux"
        ? "请填写 systemd 单元名"
        : "请填写 launchd 标签",
  logPlaceholder:
    OS === "windows"
      ? "D:\\ProgramData\\MySQL\\MySQL Server 8.0\\Data\\mysqld.err"
      : OS === "linux"
        ? "/var/log/mysql/error.log"
        : "/usr/local/var/log/mysqld.log",
  cwdPlaceholder:
    OS === "windows"
      ? "D:\\code\\myproj"
      : OS === "linux"
        ? "/home/你的用户名/code/myproj"
        : "/Users/你的用户名/code/myproj",
  commandPlaceholder:
    OS === "windows"
      ? "python / node / go / 绝对路径 exe"
      : OS === "linux"
        ? "python3 / node / go / 绝对路径可执行文件"
        : "python / node / go / 绝对路径可执行文件",
  orphanGuard:
    OS === "windows"
      ? "Job Object 兜底，防止孤儿进程占端口"
      : "进程组兜底，防止孤儿进程占端口",
};
