# 测试方案 — service-panel

生成方式：`plan-tests` skill + `node audit.cjs`（2026-10-16，本机 Windows）。
本文件**随仓库分发**（不同于 `KNOWN_ISSUES.md`，那个被 `.gitignore` 排除），
因为第 7 节的 mac 清单要在 Mac 上看。

## 0. 项目事实（体检输出）

- 栈：`Tauri 桌面(有 tauri.conf.json) | Web 前端(react)`，源码 67 个文件
- 已有测试入口：`cargo test`；**npm scripts 里没有任何测试命令**（只有 dev/build/preview/tauri）
- 已有测试文件：Rust `#[test]` 11 个文件 / JS-TS 0 个 / 疑似 E2E 0 个
- 界面证据能力：UI 组件 7 个（`src/components/`；体检报 9 是把 `App.tsx`/`main.tsx` 也算进来了），vitest 未装，Playwright 未装，Testing Library 未装
- CI：无 `.github/workflows`，远端在 gitee

## 1. 判据1 — 一条命令替你判

**后端：有。** 命令与本轮实测结果：

```bash
cd /d/code/develop/gx/mianban/service-panel/src-tauri && cargo test --lib
```
```
test result: ok. 39 passed; 0 failed; ... finished in 2.44s   EXIT=0
```
（这是 T1 补测试**之前**的基线输出；补完后为 `42 passed`，见第 8 节。）

它覆盖：日志解码(GBK/UTF-8)、端口表解析、健康 URL 解析、探测排期、优雅停止超时、
服务 PID 归属、依赖排序、日志分片滚动、密钥前缀识别、托盘签名。
**不覆盖**：DB 迁移、导入导出、IPC 命令层、整个前端。

**前端：没有。** 判定见判据4 —— 组件只有 7 个，**不为此装测试栈**，改用浏览器取证。

通过标准（以后每次改完跑这两条，都要 EXIT=0）：

```bash
cd /d/code/develop/gx/mianban/service-panel/src-tauri && cargo test --lib && cargo clippy --all-targets
```

（clippy 当前有 2 条历史告警：`group.rs:251`、`manager.rs:468`；新增告警才算坏。）

## 2. 判据2 — 边界出口

**出口极干净**：整个前端只有 `src/lib/api.ts` 一个文件 import `@tauri-apps/api`，
里面是 25 个 `invoke` + 4 个 `listen`，全部集中在一个 `api` 对象上。

→ 结论：前端任何一层测试只需 mock 这一个模块（`vi.mock("@tauri-apps/api/core")`），
不需要先做收口重构。本项目**已经**收口好了，这是最省事的一种状态。

Rust 侧的出口是 `commands.rs`（IPC 入口）与 `drivers/*`（真系统调用）。
真系统调用**不 mock**——见 L2，跨平台项目的价值恰恰在打真 API。

## 3. 判据3 — 本机验不了的代码

体检：`Rust cfg 分支 unix 侧 28 处 / windows 侧 31 处`，且**无 CI**。

含义（这条禁止靠读代码下结论，必须机器判）：
本机 `cargo test` 全绿只证明 windows 分支好；`#[cfg(unix)]` 的 28 处在 Windows 上**根本不参与编译**，
mac 能不能编译、优雅停止在真 bash 上等不等，本机永远验不了。

两条出路，选后者：

- （不选）CI 矩阵 `macos-latest`：mac runner 计费约为 Linux 的 10 倍，私有仓库按分钟算额度；
  当前项目单人开发，收益不如直接在手上那台 Mac 跑一次。
- （选）**攒一批可粘贴命令去 Mac 一次过** → 第 7 节。

交叉编译只做「能不能编译」的廉价预检，可在 Windows 先试（失败就跳过，不阻塞）：

```bash
cd /d/code/develop/gx/mianban/service-panel/src-tauri && rustup target add aarch64-apple-darwin && cargo check --target aarch64-apple-darwin 2>&1 | tail -30
```

预期：可能在 `rusqlite` bundled（C 编译需 Apple SDK）处失败 —— 那不算回归，
就退回第 7 节在 Mac 上验。**不要因为这条红了去改依赖。**

## 4. 判据4 — 界面证据

7 个组件 < 30 → **不上组件测试栈**，改用「浏览器截图 + 数值断言」。

**已实现（2026-10-16）**：桩做在 `api.ts` 接缝上，不是原计划的 `main.tsx` 里伪造
`__TAURI_INTERNALS__`——接缝只有一处，直接换实现更稳，也不用猜 Tauri 内部结构。

- `src/lib/devPreview.ts`：假后端样本，刻意覆盖边界（正常数字 / 取不到指标 / 不健康 / 配置损坏 / 已停止）
- `src/lib/api.ts`：`DEV 构建 && !("__TAURI_INTERNALS__" in window)` 时切到假 api，4 个 `listen` 返回空实现
- 生效条件双重把关，真壳与打包版进不来；`npx tsc --noEmit` 干净（顺带补了缺失的 `src/vite-env.d.ts`）

取证方式：`npm run dev` → 浏览器 `http://localhost:5273` → `evaluate_script` 量 `getBoundingClientRect`。
浏览器窗口只有 543×745（低于产品 `minWidth: 900`），所以宽窗用 `zoom=0.44` 折算成 1234 CSS px 布局量，数值再除回。

| 取证项 | 断言 | 实测结果 |
|---|---|---|
| 取不到指标显示 `—` | 卡片文本含 `CPU —`/`MEM —`，且**没有**任何卡片出现 `CPU 0%` | `MySQL84（系统服务）… CPU — MEM —`、`搜索（Docker）… CPU — MEM —`；`any_CPU_zero=false` |
| 正常数字没被误伤 | 进程型服务显示真实数字 | `API 网关 … CPU 1% MEM 43MB`（`1.4→1`、`42.5→43`，`toFixed(0)`） |
| 不健康三要素 | `dot` 类名含 `unhealthy` + 文案「不健康」+ `.err` 行可见 | `dot unhealthy` / 「不健康」 / 「健康检查连续 3 次失败（tcp 127.0.0.1:9009 连接被拒绝）」 |
| 配置损坏置灰 | 「启动」按钮 `disabled` | 报表（配置损坏）卡片启动按钮置灰，红字提示可见 |
| 同行卡片等宽 | 同一 `top` 的卡片宽度差 ≤1px | 1234px 布局下每行 2 张，宽度全为 372px，`spread=0` |
| 按钮不溢出卡片 | 所有按钮 `right/bottom` 不超出卡片边界 +1px | 溢出项 0 条 |
| 启停真的走通 | 点「停止」→ 下一轮轮询后状态与指标行变化 | 3.2 秒后 `已停止`、`dot stopped`、CPU 行消失（轮询 2500ms） |
| 编辑对话框不超视口 | `.modal` 的 `left/right/bottom` 在视口内 | `top 45 / bottom 700`，视口高 745 → 未超出 |
| 事件时间线有数据 | `.event-row` 条数 > 0 | 3 条，首行「2026/10/16 18:11:37 爬虫 运行中 → 不健康 健康检查连续 3 次失败」 |

**生产包不被污染（grep 最终产物，不看配置）**：`npm run build` 退出码 0，
`dist/assets/*.js` 里搜 `9009` / `静态站` / `API 网关` / `MySQL84（系统服务）` 全部 **0 命中**，
而 `list_services` / `port_audit` / `restart_as_admin` 仍在。
（第一版没做到：假样本模块顶层有函数调用 → Rollup 判有副作用摇不掉，产物里留着 1.1 kB 死码；
改成懒初始化 `ensure()` + `events()` 后才干净。这条坑记在这里，别下次再踩。）


**已修的 UX bug（2026-10-16，本轮发现即改）**：`.modal` 是 `max-height` + `overflow-y:auto`，
编辑表单内容 `scrollHeight=1277px` 高于可视区 → 「取消/保存」原本在首屏外（未滚动时按钮 `top=1261`，
对话框下沿 700），必须滚到底才点得到。改法：`.modal .footer` 加
`position: sticky; bottom: -20px; padding-top: 10px; background: var(--bg-elev);`
（`-20px` 抵消 `.modal` 自身 padding，让按钮贴住内边距外沿）。

改后同一窗口（视口高 745，`.modal` `top 45 / bottom 700`）数值断言：

| 断言 | 判据 | 实测 |
|---|---|---|
| 不滚动就能点保存 | 未滚动时「保存」`bottom` 在 `.modal` 可视区内 | `top 668 / bottom 699`，在视口内 ✅ |
| 滚动中途也常驻 | 滚到中段按钮位置不变 | 仍为 `top 668` ✅ |
| 滚到底不挡最后一个字段 | 末字段 `bottom` 小于 footer `top` | 末字段 `bottom 617` < footer `top 638` ✅ |
| 各对话框都受益 | 设置/时间线/管理分组/添加服务 footer 首屏可见 | 4 个全部 `visible_without_scroll: true` ✅ |

截图证据：`%TEMP%\sp-t3-list-1240.png`（列表：`—` / 不健康 / 置灰）、`sp-t3-dialog-top.png`、`sp-t3-dialog-bottom.png`。
粘底改动只有数值断言，没有截图（内置浏览器面板当时不可见，`take_screenshot` 报 `visibilityState=hidden`；
下次面板可见时补一张 `sp-t3-footer-sticky.png`）。


## 5. 判据5 — 风险点（只挑两处，且都是零测试文件）

体检线索里 `金额/价格字样: supervisor.rs` 是**误报**（`PROBE_CONCURRENCY` 含子串 "currency"），排除。

| 风险点 | 为什么贵 | 单独一层怎么测 |
|---|---|---|
| `db.rs` 迁移：`PRAGMA user_version` v1→v2，含 `ALTER TABLE groups DROP COLUMN start_order`（`db.rs:70-79`） | 用户机器上的**老库**升新版读不了 = 配置全丢，且不可回滚 | L2：临时目录手写 v1 库（`user_version=1` + `groups(id,name,start_order)` 塞一行数据）→ 调 `db::open(dir)` → 断言 `user_version==2`、`start_order` 列已消失、那行数据还在。**当前 `db.rs` 一个测试都没有**，这条是本方案最该先补的 |
| `export.rs` 导出：文件里 spec 是**明文 JSON（含 env 密钥）**（源码第 1 行自己就这么写的） | 这份文件发给别人 / 误提交进仓库 = 凭据泄漏，不可撤销 | L2 两条：① 真临时文件 round-trip，断言导出条数=导入条数、`env` 字段逐字符还原（这是功能正确性）；② **防泄漏**用一条可粘贴检查代替测试：`cd /d/code/develop/gx/mianban/service-panel && git ls-files | grep -iE "export.*\.json$|services.*\.json$"` 必须**无输出**（有输出说明把含密钥的导出样本提交了） |

其余功能从简，不为它们建脚手架。

## 6. 分层取舍

| 层 | 做什么 | 本轮 | 不做的理由 |
|---|---|---|---|
| L1 纯逻辑单测 | 已有 39 条，无 I/O 函数 | 维持，改坏才补 | 覆盖已够（解析/排期/排序/队列） |
| L2 真环境集成 | 真进程 / 真 SQLite / 真系统 API | **本机 2 条已补并跑绿（42 passed）**；Mac 上还有第 7 节那一批 | 这是替代「你手点验收」的那一层，价值最高 |
| L3 上层组件测试 | mock `api.ts` 测渲染 | **不做** | 组件仅 7 个，装 vitest+jsdom+testing-library 的收益低于成本；出口虽干净，仍由 L4 覆盖显示规则 |
| L4 界面证据 | dev 桩 + 浏览器截图 + 量尺寸 | 做，4 项固定清单 | 不需要真壳，成本最低且能钉死尺寸问题 |
| L5 E2E 驱动真壳 | Playwright + Tauri driver | **不做** | 装 driver、脆、慢；核心流程改动频率低，L2 已覆盖系统边界 |

**本轮明确不做**：不装任何前端测试依赖；不上 Playwright/E2E；不建 CI 矩阵；
不动 Docker `-t` 透传、系统服务 30s 下限、Unhealthy→重启（那三项等你定口径）。

## 7. Mac 统一验证清单（本机验不了的，一次过）

在 Mac 终端里**整段粘贴**，跑完把最后一段输出发回来即可，不用逐条截图：

```bash
cd ~/你的路径/service-panel/src-tauri \
  && echo "=== 1. 编译+单测（unix 分支参与编译）===" \
  && cargo test --lib 2>&1 | tail -5 \
  && echo "=== 2. 优雅停止超时是否真按配置等待 ===" \
  && echo '把下面两行贴进面板：命令 /bin/bash  参数 -c "trap \x27\x27 TERM; sleep 120"  停止超时 5 秒' \
  && echo "期望日志（服务日志里出现这两行才算生效）：" \
  && echo "  [panel] 优雅停止：请求进程退出，最多等待 5 秒" \
  && echo "  [panel] 5 秒内进程未退出，已强杀（实际用时 5.x 秒）" \
  && echo "两行都没有 → 不是面板拉起的进程，别当超时失效报" \
  && echo "=== 3. 健康检查间隔 ===" \
  && echo "填 interval=30, threshold=3 → 判不健康应约 90 秒后出现，不是 6 秒" \
  && echo "=== 4. unix 崩溃看门狗 ===" \
  && echo "面板被 kill -9 后，托管子进程树应在 ~2 秒内被回收"
```

| 编号 | 验什么 | 期望看到 | 不符合时发回 |
|---|---|---|---|
| M1 | `cargo test --lib` | `42 passed; 0 failed`（Windows 本机本轮数；unix 分支编译过即算 M1 通过） | 编译报错前 30 行 |
| M2 | Command 驱动优雅停止 | 第 6 节那两行 `[panel]` 日志，且「实际用时」≈ 配置的 5 秒 | 服务日志全文 + 面板日志目录 `ls -l` |
| M3 | 系统服务(launchctl)停止 | 走 `bootout`，真实宽限由 plist `ExitTimeOut` 决定，**面板的超时不生效属设计** | 只发日志，不用调 |
| M4 | Docker 停止 | 用 Docker 自身 10 秒（面板没传 `-t`），与配置无关 | 日志 |
| M5 | 健康检查间隔生效 | 不健康判定推迟到 `间隔×阈值` | 事件时间线截图 |
| M6 | CPU/内存为 0 的外部服务 | 显示 `—` 而非 `0` | 卡片截图 |

## 8. 验收

- [x] 判据1 命令本轮跑通：`39 passed; 0 failed`，`EXIT=0`（输出见第 1 节）
- [x] L2 本机两条已补并跑绿：**42 passed / 0 failed, EXIT=0**（`db::tests::v1_db_upgrades_without_losing_rows`、`export::tests::export_then_import_restores_env_values_verbatim`、`export::tests::export_aborts_when_any_service_config_is_unreadable`）
- [x] 三条都验过「真会红」：注释掉 `ALTER TABLE groups DROP COLUMN` → 迁移测试红；`enabled: s.enabled` 改成 `false` → round-trip 红；关掉导出中止保护 → abort 测试报 `unwrap_err() on Ok(2)`（即「坏配置被当真实配置写进备份」正是它守的场景）。全部改回后重新跑绿
- [ ] 迁移测试暴露的一个真实口径：`ServiceSpec.driver` **没有** `#[serde(default)]`，所以字段不全的 spec JSON 会被判「配置损坏」。老库里真实的明文记录是完整 JSON，故不影响升级；但**手工编辑/新版加字段**时要注意这点（要不要补 default 由你定，本轮未改产品代码）
- [x] L4 桩已加并在浏览器里取证：8 项断言全部拿到数值（第 4 节表格），截图 3 张在 `%TEMP%\sp-t3-*.png`；`npx tsc --noEmit` 退出码 0
- [x] 查到的 UX bug 已当场修：编辑对话框「保存/取消」原本在首屏外（`top=1261`），
      `.modal .footer` 改 sticky 后不滚动即可见（`top 668 / bottom 699`），4 个对话框全覆盖，数值见第 4 节
- [ ] Mac 清单去跑之前确认本文件在仓库里（`git ls-files docs/TEST_PLAN.md` 有输出），`KNOWN_ISSUES.md` 不在
- [ ] 改配置类（`vite.config.ts` 端口 / `tauri.conf.json`）后要 grep 生成物确认，不看配置本身
