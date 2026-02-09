<p align="center">
  <a href="https://vibebox.robcholz.com">
    <picture>
      <img src="docs/banner.png" alt="VibeBox logo">
    </picture>
  </a>
</p>
<p align="center">超高速、开源的 AI 沙盒。</p>

<p align="center">
  <a href="https://crates.io/crates/vibebox">
    <img alt="Crates.io" src="https://img.shields.io/crates/v/vibebox.svg">
  </a>
  <a href="https://github.com/robcholz/vibebox/blob/main/LICENSE">
    <img alt="MIT licensed" src="https://img.shields.io/badge/license-MIT-blue.svg">
  </a>
  <a href="https://github.com/robcholz/vibebox/actions?query=workflow%3ACI+branch%3Amain">
    <img alt="Build Status" src="https://github.com/robcholz/vibebox/workflows/CI/badge.svg">
  </a>
</p>

<p align="center">
  <a href="README.zh.md">简体中文</a> |
  <a href="README.md">English</a>
</p>

VibeBox 是一个轻量、启动极快的沙盒环境，让 AI Agent 可以安全地直接跑命令、改文件、执行代码，不会不停弹“要不要允许”的提示。它基于
Apple 的 Virtualization Framework 做到彻底隔离，所以无论 Agent 在里面怎么折腾，你的真实系统都不会被影响；同时做到了低内存和磁盘占用。

[![VibeBox Terminal UI](docs/screenshot.png)](https://vibebox.robcholz.com)

---

### 我为什么做 VibeBox

我平时经常用像 Codex 和 CC 这样的
agent，但一直不太敢让它们直接在我的宿主机上跑。把权限收紧一点，就会被各种“你确定吗？”的确认弹窗不停打断；放松一点，又担心它哪天误操作，碰到不该碰的文件，或者跑了我没打算执行的命令。

我想要的是一种体验：像给 agent 一个真实 shell 一样顺滑，但同时又有一道硬隔离的安全边界。于是我做了 VibeBox：一个按项目划分的
micro-VM 沙箱，启动很快，改动被限制在仓库范围内，让我可以更高频地迭代，而不用一直盯着权限确认。

### 对比

下面是我为什么没有直接用现成方案的原因：

- **vibe**：非常方便，但对我来说太“极简”了。它缺少一些基础配置能力，也没法提供我工作流里需要的多开和 session 管理。
- **QEMU**：很强大，但配置面太大了。日常当沙箱用，它不像是“进到 repo 就能用”，更像是你得先把它当成一个项目来折腾。
- **Docker / devcontainers**：生态很成熟，但日常使用对我来说偏重。冷启动有时会慢，而且它不是那种我能一天反复、随时秒进秒出的工具。

这些就促使我做了 **VibeBox**：我想要一个按项目隔离的沙箱，进入速度快（直接用 `vibebox`），支持真正可用的配置和
sessions，同时还保留明确的硬隔离边界。

### 安装

```bash
# YOLO：一键安装
curl -fsSL https://raw.githubusercontent.com/robcholz/vibebox/main/install | bash

# Cargo
cargo install vibebox

# 或者手动安装
curl -LO https://github.com/robcholz/vibebox/releases/download/latest/vibebox-macos-arm64.zip
unzip vibebox-macos-arm64.zip
mkdir -p ~/.local/bin
mv vibebox ~/.local/bin
export PATH="$HOME/.local/bin:$PATH"
```

**系统要求**

- Apple Silicon 的 macOS（VibeBox 使用了 Apple 的虚拟化 API）。

**首次运行**

第一次执行 `vibebox` 会下载 Debian 基础镜像并完成初始化。之后每个项目的实例会复用缓存的基础镜像，
启动会快很多。

### 文档

**快速开始**

```bash
cd /path/to/your/project
vibebox
```

第一次运行时，如果项目目录里缺少配置，VibeBox 会自动创建 `vibebox.toml`（放在项目根目录），并创建
`.vibebox/` 用来保存实例数据。

**配置（`vibebox.toml`）**

默认情况下，`vibebox.toml` 位于项目根目录。你可以用 `vibebox -c path/to/vibebox.toml` 或设置
`VIBEBOX_CONFIG_PATH` 环境变量来覆盖路径，但配置文件必须仍然位于项目目录内部。

默认配置（缺失时会自动生成）：

```toml
[box]
cpu_count = 2
ram_mb = 2048
disk_gb = 5
mounts = [
    "~/.codex:~/.codex:read-write",
    "~/.claude:~/.claude:read-write",
]

[supervisor]
auto_shutdown_ms = 20000
```

注意：`disk_gb` 只在「首次创建实例磁盘」时生效。之后如果你改了它，需要运行 `vibebox reset` 重新创建磁盘。

**挂载（Mounts）**

- 你的项目会以读写方式挂载到 `~/<project-name>`，并且 shell 会默认从那里启动。
- 如果项目里存在 `.git` 目录，VM 内会用 tmpfs 把它遮住，避免你在 guest 里误操作改到 Git 元数据。
- 额外挂载通过 `box.mounts` 配置，格式为 `host:guest[:read-only|read-write]`。
- Host 路径支持 `~` 展开；guest 的相对路径会被视为 `/root/<path>`。
- guest 路径如果用了 `~`，会为了方便被链接到 `/home/<ssh-user>` 下。你可以运行 `vibebox explain`
  查看最终解析后的 host/guest 映射关系。

**CLI 命令**

```bash
vibebox             # 启动或连接当前项目的 VM
vibebox list        # 列出已知的项目会话
vibebox reset       # 删除当前项目的 .vibebox，下一次运行会重新创建
vibebox purge-cache # 删除全局缓存（~/.cache/vibebox）
vibebox explain     # 显示挂载与网络信息
```

**在 VM 内部**

- 默认 SSH 用户：`vibecoder`
- 主机名：`vibebox`
- 基础镜像初始化会安装：构建工具、`git`、`curl`、`ripgrep`、`openssh-server`、`sudo`
- 首次登录时，VibeBox 会安装 `mise`，并尽力配置 `uv`、`node`、`@openai/codex`、
  `@anthropic-ai/claude-code` 等工具（best-effort，视网络和环境而定）
- Shell 里有两个别名：`:help` 和 `:exit`

**状态与缓存**

- 项目级状态在 `.vibebox/`（实例磁盘、SSH key、日志、manager socket/pid）。`vibebox reset` 会移除它。
- 全局缓存在 `~/.cache/vibebox`（基础镜像 + 共享 guest 缓存）。`vibebox purge-cache` 会清空它。
- 会话索引在 `~/.vibebox/sessions`，可以通过 `vibebox list` 查看。

### 参与贡献

如果你想参与贡献 VibeBox，请先阅读 [贡献指南](CONTRIBUTING.md)，再提交 Pull Request。

### 使用 VibeBox

欢迎使用

### FAQ

#### 它和其它 Sandboxes 有什么不同？

Vibebox 追求的是：本地、可复现、启动快、流程简单。主要差异点：

- 在我的 M3 上，热启动通常 **6 秒以内**，可以非常快地回到工作状态。
- 一个命令——`vibebox`——直接把你带进沙盒（从你的项目目录启动）。
- 配置集中在 `vibebox.toml`，CPU / 内存 / 磁盘大小 / 挂载都能一眼看懂、随手改。

### 特别鸣谢

[vibe](https://github.com/lynaghk/vibe) by lynaghk。

以及 Rust 社区。没有你们丰富的 crates 生态和优秀的工具链（比如 [crates.io](https://crates.io)），
这个项目不可能这么顺利！Rust教。

---

**在 X 上关注我** [X.com](https://x.com/robcholz)
