# 本地服务管理面板 (Service Panel)

一个跑在 Windows 上的桌面面板，统一管理本机 Python / Node.js / Go 服务，以及
MySQL、Redis 等中间件（无论它们是绿色版进程、Windows 服务，还是 Docker 容器）。
一键启动 / 停止 / 重启、实时日志、健康检查 + 崩溃自动拉起、资源监控。

技术栈：**Tauri 2 (Rust) + React 18 + TypeScript + SQLite**。

## 环境要求

- Rust（stable，MSVC 工具链）——`rusqlite` 用 `bundled`，需要可用的 C 编译器/linker
- Node.js 18+（自带 npm 即可，本项目用 npm）
- Windows：MSVC Build Tools（装 Rust 时选 `x86_64-pc-windows-msvc` 一般已就绪）
- 管理 Windows 服务需要**管理员权限**；需要以管理员运行面板

## 开发运行

```bash
cd service-panel
npm install --registry=https://registry.npmmirror.com   # 国内网络建议用镜像源
npm run tauri dev    # 首次会编译 Rust，较慢；之后热更新
```

前端 dev 端口固定为 **5273**（在 `vite.config.ts` 和 `src-tauri/tauri.conf.json`
的 `devUrl` 两处，改端口要同时改）。故意不用 vite 默认的 5173，避免和你机器上
其它项目的 dev server 撞端口。

## 常见报错

- **`Port 5173 is already in use` / `beforeDevCommand terminated`**
  本项目已改用 5273。若 5273 也被占，把 `vite.config.ts` 的 `port` 和
  `tauri.conf.json` 的 `devUrl` 一起换成另一个空闲端口即可。

- **`npm install` 卡住不动**
  默认 `registry.npmjs.org` 在部分网络下很慢或不通，用镜像源：
  `npm install --registry=https://registry.npmmirror.com`。

- **esbuild 报 `install scripts blocked` / vite 起不来提示 esbuild 二进制缺失**
  安全策略会拦 postinstall，批准并重建即可：
  `npm install-scripts approve esbuild && npm rebuild esbuild`。

- **首次 `cargo build` 很慢**
  正常，要下 ~400 个 crate 并编译 bundled SQLite，之后走缓存就快了。

- **管理 Windows 服务时 `sc start` 报权限错误**
  需要管理员权限，以管理员身份运行面板。

## 打包发布

```bash
npm run tauri build
# 产物在 src-tauri/target/release/bundle/
```

图标占位文件已生成在 `src-tauri/icons/`，要换自己的 logo：
把源图放好后运行 `npm run tauri icon <你的.png>`，或 `node scripts/gen_icons.mjs`。

## 目录结构

```
service-panel/
  src/                     React 前端
    App.tsx                主布局 + 事件订阅 + 轮询
    types.ts               与 Rust 对齐的类型
    lib/api.ts             invoke 封装 + 事件监听
    lib/deps.ts            依赖拓扑分层 + 环检测（供依赖图）
    components/            ServiceCard / LogConsole / AddServiceDialog
                           GroupManagerDialog / SettingsDialog / DependencyGraph
  src-tauri/               Rust 后端
    src/
      types.rs             领域类型（ServiceSpec / State / 重启策略…）
      db.rs                SQLite 迁移与仓储
      drivers/             驱动抽象
        command.rs         CommandDriver + Windows Job Object（进程树回收）
        winservice.rs      Windows 服务（sc start/stop/query）
        docker.rs          Docker（docker start/stop/inspect）
      logs.rs              日志读取线程 + 落盘 + 前端广播
      logsrc.rs            外部服务日志接入（docker logs -f / 文件尾随）
      health.rs            TCP / HTTP / 命令 健康检查
      state.rs             AppState / Runtime 状态机
      manager.rs           start / stop / restart 编排 + 生命周期互斥锁
      group.rs             分组拓扑排序（Kahn）+ 按序启停编排
      supervisor.rs        后台监督线程：崩溃检测、健康、重启退避、资源采样
      secrets.rs           配置密文存储（Windows DPAPI）
      export.rs            配置导出/导入（明文 JSON，跨机迁移通道）
      winjob.rs            Job Object 共享封装（托管进程 + docker 尾随进程复用）
      tray.rs              系统托盘（菜单、动态刷新、事件）
      commands.rs          暴露给前端的 Tauri Command
      lib.rs               装配（插件注册、注入 emitter、启动监督、注册命令）
  scripts/gen_icons.mjs    零依赖生成占位图标
```

## 当前骨架已完成 / 待办

已完成（M1–M5）+ 增强：三类驱动、Command 托管（含 Job Object 进程树回收）、实时日志流、
状态机 + 期望态分离、健康检查、指数退避自动重启、资源采样、SQLite 持久化、
添加/编辑向导 UI、全部启停、事件时间线接口。
**M4 新增**：开机自启（`tauri-plugin-autostart`）、单实例锁（`tauri-plugin-single-instance`）、
系统托盘常驻（左键切换窗口、右键菜单逐服务启停/全部启停/退出）、关闭窗口最小化到托盘、
设置面板的开机自启开关。
**M5 新增**：分组一键启停 + 依赖拓扑排序（`group.rs` 用 Kahn 算法按 `spec.after` 边定序并检测循环依赖）；
组内启动按拓扑顺序逐个"启动 + 就绪等待（TCP/HTTP 健康探通）"再启下一个，任一步失败即中断整条链并经
`group://result` 事件回报；停止按逆序（先起的后停）。"全部启动/停止"也走全局拓扑定序。
前端加分组标签栏、本组启停按钮、分组管理弹窗、服务归属分组下拉。

下一步（可选增强）：
- HTTP 健康检查支持 https；`sc`/`docker` 输出的多语言解析加固
- 资源历史曲线（**评估后暂缓**：个人本机工具 ROI 低，长期监控更适合 Prometheus+Grafana；
  实时数值 + 日志 + 自动重启已覆盖"当前是否健康"）

## 依赖关系图

- 纯前端 SVG，无第三方图论/图表库：`lib/deps.ts` 用 Kahn 拓扑 + 最长路径给服务分层，
  `components/DependencyGraph.tsx` 按层排布、贝塞尔箭头连线（方向 = 启动先后，被依赖 → 依赖方）。
- 环检测：无法入队的节点即陷入循环依赖，红框 + 红色连线 + 顶部告警标出。
- 与分组联动：在某个分组标签下点「依赖图」只看该组；「全部」看全局。节点带实时状态圆点。
- 数据源就是服务里勾选的「依赖服务」（`spec.after`），和"本组启动"用的是同一套拓扑，所见即所跑。

## 环境密钥加密存储

- 整条 `spec_json`（含 env 里的 DB 密码、API key）写库前用 **Windows DPAPI（CryptProtectData）**
  加密，密文与「当前用户 + 本机」绑定，无需外部密钥文件；读取时解密。
- 带 `enc:` 前缀自描述，**兼容旧明文记录**（无前缀按明文解析，下次保存自动升级为加密）。
- 加密失败时降级为明文（不阻断使用）。
- 注意：DPAPI 是机器+用户绑定的，`panel.db` 拷到别的机器/换 Windows 用户会解不开——
  跨机迁移请在新机重新配置，别指望直接拷库。
- 平台限定：本项目按 Windows 单平台设计（DPAPI + Job Object）。若要出 Mac/Linux 版，
  需把加密换成 `keyring`+AES-GCM、进程树回收换成 `setpgid`/`kill(-pgid)`。

## 配置导出 / 导入（跨机迁移通道）

- 设置面板 →「配置备份」→ 导出/导入。走原生文件对话框（`tauri-plugin-dialog`）。
- 导出文件是**明文 JSON**（`version / exportedAt / appVersion / groups / services`），
  含服务定义、分组、依赖关系与 env 密钥——请像对待密码管理器一样保管它。
- 导入两种模式：
  - **合并**：保留现有配置，只添加文件中不存在的 id（同 id 跳过）。
  - **覆盖**：先停所有正在运行的服务、清空 services/groups/events 三张表，再按文件重建。
- 导入走后台线程（覆盖模式要挨个优雅停，可能耗时），完成后自动刷新托盘。
- 为什么需要它：DPAPI 是**机器+用户绑定**的，`panel.db` 直接拷到别的电脑或换用户登录
  都解不开。跨机迁移、备份恢复、换硬件，全靠这个"导出（解密）→ 拷 JSON → 导入（重新加密入库）"通道。

## Windows 服务 / Docker 日志接入

- Docker：服务运行时自动 spawn `docker logs -f --tail 200` 作为尾随子进程，复用同一套
  日志管线（UI 广播 + 本地落盘）。该子进程挂在 Job Object 上，面板退出即被回收，不留孤儿。
- Windows 服务：无标准输出，改为尾随服务写的日志文件——在服务的"日志文件路径"里填
  该文件绝对路径（如 MySQL 的 `mysqld.err`）。首次只回看最后 ~64KB，之后每秒增量读取，
  支持文件轮转（size 变小则重置偏移）。
- 命令型服务：主输出仍走 stdout/stderr 管道；若额外填了"日志文件路径"，会并行尾随该文件。
- 启停联动：`manager::start` 成功后 `logsrc::attach`，`stop`/`remove` 及监督线程检测到
  外部服务未运行时 `logsrc::detach`；面板重启接管时若外部服务已在运行也会挂上日志源。

## 体验优化（持续）

- 启停/重启/删除改为 `async` + `spawn_blocking`，重活离开主线程，界面不再因等待而发木。
- 每服务生命周期操作互斥锁（`AppState.ops` + RAII `OpGuard`）：异步化后防止双击重复
  起进程，也让监督线程的自动重启不与用户操作打架；重启失败会重新排期而非卡死。
- 日志栏支持关键字过滤 + 按 stdout/stderr/系统 流过滤、匹配计数、一键复制当前视图。
- 输出编码支持"自动"（UTF-8 优先、回退 GBK），解决中文 Windows 下 Python 日志乱码。
- 命令/参数/工作目录启动前统一 `trim()`，避免复制粘贴带空格导致 `os error 267`。
- Command 驱动在 Windows 上找不到程序时自动回退 `cmd /C`，`npm`/`conda` 等 .cmd 可直接跑。

## 健壮性加固（代码审查后修复）

- **健康探测不再挂死**：HTTP 探测连接/写/读全部带超时（此前只连不设读超时，
  "接受连接但永不回包"的服务会永久卡住）；命令型探测改为带超时的 spawn+轮询+超时 kill。
- **监督线程不再持锁做 I/O**：`tick` 拆成三段——锁内只做资源采样与 Command `try_wait`
  （非阻塞），外部状态查询（sc/docker）与健康探测收集为任务，**锁外并行**跑（`thread::scope`），
  再回锁应用结果。此前一次慢 `docker inspect` 或挂死的 HTTP 探测会卡住整把 runtimes 锁，
  连带堵死 list_services、托盘刷新、所有启停。
- **配置损坏不再静默清空备份**：`unpack_spec` 解密/解析失败会标记 `config_ok=false` 并在卡片
  显示告警；导出前检测到坏行会**中止导出**，避免把空 spec 当真实配置写进备份。
- **Windows 服务停止按异步处理**：`sc stop` 返回后轮询 `STATE` 直到真正 STOPPED（或超时），
  不再立刻置 Stopped 导致用户点启动撞 1056/1062（MySQL 这类要十几秒停的服务尤为关键）。
- **sc 输出解析 locale 无关**：状态判定改为从原始字节里抽 `STATE` 后的数字码（RUNNING=4/STOPPED=1），
  不再依赖 `contains("4")` 这种会被其它数字误命中的脆弱判断；错误信息按 GBK 解码。

### 第二轮加固（性能与自愈）

- **轮询不再重复解密**：`list_services` 改为静态列走 DB、spec 与动态状态走内存 `Runtime`（启动时解密一次并缓存），
  去掉每 2.5s 对全部服务做一次 DPAPI 解密的开销。
- **日志批量传输**：后端按 80ms 聚合成数组一次 `emit("log://batch")`，前端一批只 setState 一次；
  日志栏只渲染最后 800 行。话痨服务（Vite/Python 循环）不再造成每秒上千次 IPC + 数组拷贝 + 重渲染。
- **重试计数自愈**：启动成功对所有驱动重置 `fail_streak`；稳定运行超 30s 自动清零；
  超过 `max_retries` 不再永久放弃，而是进入 5 分钟冷却后继续重试（补齐设计文档"最大重试 + 冷却"）。
- **进程树采样含孙进程**：每 tick 建一次 parent→children 映射做 BFS，Node/pm2 的 worker 不再被漏算。
- **分组批量启停互斥 + 进度**：`group_ops` 集合防止连点起多条链抢锁；逐服务推 `group://progress`
  事件，前端 toast 显示"启动中 2/5 · 服务名"，不再一条链静默最多 45 秒。

## 关键设计点

- **Job Object（KILL_ON_JOB_CLOSE）**：面板崩溃或被强杀时，被托管的进程树由操作系统
  回收，不会遗留孤儿进程占用端口。这是自研面板相对普通脚本的核心价值。
- **期望态 vs 真实态分离**：`desired` 与 `state` 分开存，保证用户手动停止的服务不会被
  重启策略又拉起来。
- **重启退避**：`base * 2^attempt`，上限 60s，配合 `max_retries` 防止无限重启风暴。
