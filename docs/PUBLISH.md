# 发布教程：GitHub + Gitee（分支开发 + Tag 发布）

本教程面向本项目（Tauri 2 桌面应用，Windows 单平台），采用 **GitHub Flow**：
`main` 稳定分支 + feature 分支开发 + 打 `v*` tag 发布。发布以 **GitHub 为主**（承载 CI 与 Release），
**Gitee 作国内镜像**。

> 约定：以下 `<GH_URL>`、`<GITTEE_URL>` 换成你自己的仓库地址。示例用户名 `DapperLee`、仓库名 `service-panel`。

---

## 0. 前置：账号与 SSH 密钥

推荐用 SSH，配一次长期免密。

```bash
# 生成密钥（已有一把可跳过）
ssh-keygen -t ed25519 -C "你的邮箱" -f ~/.ssh/id_ed25519
# 打印公钥，复制整行内容
cat ~/.ssh/id_ed25519.pub
```

- GitHub：Settings → SSH and GPG keys → New SSH key，粘贴公钥。
- Gitee：设置 → SSH 公钥 → 新增，粘贴同一把公钥。

验证：

```bash
ssh -T git@github.com     # 出现 Hi xxx! 即成功
ssh -T git@gitee.com      # 出现 Hi xxx! 即成功
```

> 不想用 SSH 也可以用 HTTPS + Personal Access Token（GitHub 已不支持账号密码）。
> GitHub：Settings → Developer settings → Personal access tokens，勾 `repo` 权限。
> Gitee：设置 → 私人令牌，勾 `projects` 权限。推送时用户名填账号、密码填令牌。

---

## 1. 把默认分支改成 main

```bash
cd /d/code/develop/gx/mianban/service-panel
git branch -m master main
```

---

## 2. 在两边各建一个「空仓库」

到 GitHub 和 Gitee 网页 New repository：

- 仓库名都填 `service-panel`；
- **不要勾选** 自动初始化 README / .gitignore / License（否则远程历史和本地冲突）；
- 可见性随意（注意：本项目导出文件含明文密钥，仓库里不要提交任何 `.db`/日志/导出 json）。

建好后复制两个地址：
- `git@github.com:DapperLee/service-panel.git`
- `git@gitee.com:DapperLee/service-panel.git`

---

## 3. 添加两个远程并首次推送

```bash
git remote add github git@github.com:DapperLee/service-panel.git
git remote add gitee  git@gitee.com:DapperLee/service-panel.git

git push -u github main
git push -u gitee  main
```

---

## 4. 日常开发：在分支上写，合并进 main

```bash
# 从最新 main 开一个功能分支
git switch main
git pull github main
git switch -c feature/xxx

# ...开发、提交...
git add -A
git commit -m "feat: 说明"

# 完成后合并回 main（--no-ff 保留分支拓扑，便于回溯）
git switch main
git merge --no-ff feature/xxx -m "merge: feature/xxx"
git branch -d feature/xxx

# 同步两边
git push github main
git push gitee  main
```

> 想更规范：feature 分支 push 到 GitHub 后开 Pull Request，自己 review 再合并。

---

## 5. 发布一个版本（核心）

### 5.1 统一改版本号

三处保持一致（示例 `0.2.0`）：
- `package.json` → `"version"`
- `src-tauri/Cargo.toml` → `[package] version`
- `src-tauri/tauri.conf.json` → `"version"`

```bash
git add -A
git commit -m "chore: bump version to 0.2.0"
```

### 5.2 本地打出安装包（可选，用于自测/手动上传）

```bash
npm run tauri build
# 产物：src-tauri/target/release/bundle/ 下的 .msi / .exe（nsis）
```

当前配置（`tauri.conf.json` → `bundle.windows`）已把两种安装包都设为简体中文界面：

- `nsis.languages: ["SimpChinese"]` → `nsis/service-panel_0.2.0_x64-setup.exe`
- `wix.language: "zh-CN"` → `msi/service-panel_0.2.0_x64_zh-CN.msi`（文件名里的语言段随配置变，改之前打的是 `*_en-US.msi`）

> 注意：切换语言/版本后重打，`bundle/msi/` 里会同时留着旧的 `*_en-US.msi`，上传前按文件名挑，别拿错。
> 只重打一种：`npm run tauri build -- --bundles msi`（或 `nsis`）。

### 5.3 打 tag 并推送（tag 即"发布"）

发布内容以 `main` 为准：先在 Gitee 网页发 PR 把 `develop` 合进 `main`，本地同步后再打 tag。

```bash
git fetch gitee
git checkout main && git merge --ff-only gitee/main && git checkout develop   # 拿到网页合并结果
git tag -a v0.2.0 -m "v0.2.0 说明" main
git push github main --follow-tags
git push gitee  main --follow-tags
```

> **github 远程若是 HTTPS 地址，推 tag/commit 需要 PAT**（GitHub Settings → Developer settings → Tokens，勾 `repo`），
> 弹登录框时用户名填账号、密码填令牌；一劳永逸的办法是换成 SSH：
> `git remote set-url github git@github.com:<你的用户名>/service-panel.git`

### 5.4 在 GitHub 建 Release

- 若配了第 6 节的 CI：推 tag 会自动构建并生成草稿 Release，你只需去 Releases 页面点 "Publish"。
- 若手动：GitHub → Releases → Draft a new release → Choose tag 选 `v0.2.0`（没有就手动输入新建，指向 main）→ 把 `bundle/` 里的两个安装包（见 5.2 文件名）拖进 "Attach binaries" → Publish release。

### 5.5 在 Gitee 建发行版

入口：Gitee 仓库首页 → 顶部菜单 **「发行版」**（不是「标签」页）→ 右上角 **「新建发行版」**：

- 分类标签：选或现填 `v0.2.0`（对应 tag 不存在时会自动创建）；
- 名称/说明与 GitHub Release 保持一致；
- 附件：上传 `..._x64-setup.exe` 和 `..._zh-CN.msi`（免费版单文件上限约 100M，本项目安装包 3~4M 无压力）；
- 保存即发布。

---

## 6. 自动化发布：GitHub Actions（tag 触发）

在仓库放 `.github/workflows/release.yml`：

```yaml
name: Release

on:
  push:
    tags:
      - 'v*'

permissions:
  contents: write

jobs:
  release:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4

      - name: Setup Node
        uses: actions/setup-node@v4
        with:
          node-version: 20

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Install frontend deps
        run: npm install

      - name: Build & Release (Tauri)
        uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
        with:
          tagName: ${{ github.ref_name }}
          releaseName: 'Service Panel ${{ github.ref_name }}'
          releaseBody: 'Windows 安装包见下方 Assets。'
          releaseDraft: true
          prerelease: false
```

之后每次 `git push github main --follow-tags` 推一个新 `v*` tag，GitHub 就会自动：
装 Node/Rust → `tauri build`（Windows）→ 创建 Release 草稿并上传安装包。你去 Releases 点 Publish 即可。

> 首次运行若报 Tauri 依赖缺失，按需在 workflow 里补 `cargo` 缓存或系统依赖；Windows runner 一般自带 MSVC，无需额外装。
> 代码签名（可选）：需要证书时再在 workflow 里配 `TAURI_SIGNING_*` / Windows 签名变量。

---

## 7. 一条命令同步两边（可选）

把 Gitee 也挂到 `github` 这个远程的 push 列表，`git push github main` 会同时推两处：

```bash
git remote set-url --add --push github git@github.com:DapperLee/service-panel.git
git remote set-url --add --push github git@gitee.com:DapperLee/service-panel.git
# 之后：git push github main --follow-tags 两边一起更新
```

---

## 8. 常见问题

- **push 被拒绝（non-fast-forward）**：远程有本地没有的提交。最常见诱因是**在 Gitee/GitHub 网页上直接改过文件、加过 LICENSE、合并过 PR**——这些操作只在远端产生提交。解决：`git fetch <远程>` 后，本地分支若是纯落后就 `git merge --ff-only <远程>/<分支>`，有本地提交就 `git pull --rebase <远程> <分支>`，再推。首次建库时若误勾了 README 初始化，也会这样——去仓库删掉那个初始提交，或 `git pull --rebase` 合并。
- **`git push gitee main` 被拒而 develop 正常**：main 是发布分支，远端可能已被网页 PR 合并推进。别强推；按上面方法先同步本地 main，日常代码只推 develop，进 main 走网页 PR。
- **GitHub 提示密码不对**：GitHub 已停用账号密码，必须用 PAT 或 SSH。
- **误把 `target/`、`node_modules/`、`.db`、导出 json 提交了**：`.gitignore` 已排除；若历史里误提交过，用 `git rm --cached <path>` 移除并重新提交（含密钥的文件务必彻底清理，必要时重写历史）。
- **main 分支保护**：GitHub 仓库 Settings → Branches 可开启 main 保护，强制走 PR；本地直推会被拒，改用分支 + PR 流程。
- **版本号与 tag 不一致**：以 tag 为准；CI 用 `github.ref_name` 作为 Release 名，确保 tag、package.json、Cargo.toml、tauri.conf.json 四处版本一致。

---

## 一页速查

```
开发：  git switch -c feature/xxx → commit → git switch main → git merge --no-ff feature/xxx
发布：  改版本号(3处) → commit → develop 走 PR 合入 main → 本地 main ff 同步 → git tag -a v0.2.0 → git push github main --follow-tags → git push gitee main --follow-tags
产物：  npm run tauri build → bundle/nsis/*_x64-setup.exe + bundle/msi/*_x64_zh-CN.msi（均简体中文界面）
Release：GitHub Releases 拖两个安装包；Gitee 顶部「发行版」→ 新建 → 传附件
CI：    推 v* tag → GitHub Actions 自动构建并生成 Release 草稿
```
