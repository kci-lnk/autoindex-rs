<h1 align="center">📂 autoindex-rs</h1>

<p align="center">
  <strong>安全、轻量、可直接分发的 Rust 目录索引服务</strong><br>
  熟悉的 <code>Index of /</code>，加上 GitHub 风格 README、完整 HTTP 文件响应和能力式文件系统边界。
</p>

<p align="center">
  <a href="https://github.com/kci-lnk/autoindex-rs/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/kci-lnk/autoindex-rs/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/kci-lnk/autoindex-rs/actions/workflows/release.yml"><img alt="Release" src="https://github.com/kci-lnk/autoindex-rs/actions/workflows/release.yml/badge.svg"></a>
  <a href="https://github.com/kci-lnk/autoindex-rs/releases/latest"><img alt="Latest Release" src="https://img.shields.io/github/v/release/kci-lnk/autoindex-rs?display_name=tag&sort=semver"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/github/license/kci-lnk/autoindex-rs"></a>
  <img alt="Rust 1.88+" src="https://img.shields.io/badge/Rust-1.88%2B-dea584?logo=rust&logoColor=white">
</p>

<p align="center">
  <a href="#快速开始">快速开始</a> ·
  <a href="#实机表现">实机表现</a> ·
  <a href="#配置">配置</a> ·
  <a href="#github-风格-readme">README 渲染</a> ·
  <a href="#安全边界">安全边界</a> ·
  <a href="#部署">部署</a> ·
  <a href="https://github.com/kci-lnk/autoindex-rs/releases/latest">下载 Release</a>
</p>

---

`autoindex-rs` 把一个本地目录变成可浏览、可下载的 Web 目录。它只服务一个目录，不包含 HTTP Host 路由、子域映射、TLS/SNI、认证、gRPC 或管理后台，适合作为独立服务运行，或放在反向代理之后。

> [!TIP]
> 下载预编译二进制后，只需一条命令即可启动：`autoindex-rs /path/to/public`。

## 特性一览

| | 能力 | 说明 |
|---|---|---|
| 📦 | **单文件部署** | 模板、CSS 和 JavaScript 全部编译进二进制，不需要运行时静态资源。 |
| 🗂️ | **完整目录索引** | 目录优先排序、双向游标分页、面包屑、默认文档、移动端布局与明暗主题。 |
| 📖 | **GitHub 风格 README** | 支持 GFM 表格、任务列表、删除线、自动链接和五类 GitHub Alerts。 |
| 📥 | **标准文件响应** | MIME、ETag、Last-Modified、条件请求、单 Range 与 multipart Range。 |
| 🔒 | **能力式安全边界** | 预打开根目录，阻止路径穿越、隐藏文件泄露和符号链接逃逸。 |
| 🌍 | **跨平台 Release** | Linux 静态 musl、macOS Intel/Apple Silicon 与 Windows x64。 |

## 实机表现

以下数据来自 `autoindex-rs v0.1.2` 的局域网实测，压测端使用 `wrk`。目标目录包含 5 个可见项目和一个约 10.3 KiB 的 README；结果用于展示资源量级，不代表所有硬件、目录规模或网络环境。

| 平台与场景 | 并发 / 时长 | 吞吐 | 平均延迟 | P99 | RSS |
|---|---:|---:|---:|---:|---:|
| 8 核 x86_64 NAS，启动后空闲 | — | — | — | — | **5.41 MiB** |
| x86_64 NAS，完整目录页 + README | 64 / 20 秒 | **244.03 req/s** | 264.76 ms | 782.55 ms | 最高 30.01 MiB |
| x86_64 NAS，1 KiB 文件 Range | 128 / 20 秒 | **3,212.31 req/s** | 39.73 ms | 67.12 ms | 最高 10.18 MiB |
| ARMv7 OpenWrt，空闲 | — | — | — | — | **2.58 MiB** |

两轮正式压测加预热累计完成 `77,426` 次请求，没有连接错误、HTTP 错误、panic 或异常日志。x86_64 测试中的内核峰值 RSS 为 `31.87 MiB`；目录页压力结束后，阻塞线程从 94 回到 9，RSS 回落至 `6.56 MiB`。本次有界测试未观察到持续内存增长。

## 快速开始

### 使用预编译二进制

从 [GitHub Releases](https://github.com/kci-lnk/autoindex-rs/releases/latest) 下载与你的平台匹配的压缩包，使用同一 Release 中的 `SHA256SUMS` 校验后解压：

```bash
./autoindex-rs /path/to/public
```

打开 `http://127.0.0.1:6701/`。默认行为如下：

| 项目 | 默认值 |
|---|---|
| 服务目录 | 当前工作目录，或位置参数指定的目录 |
| 监听地址 | `0.0.0.0` |
| 端口 | `6701` |
| README 渲染 | 开启 |
| 默认文档 | `index.html`、`index.htm` |
| 每页数量 | 100 |
| 显示时区 | `Asia/Shanghai` |

常用示例：

```bash
# 只允许本机访问，并改用 8080 端口
autoindex-rs ./public --bind 127.0.0.1 --port 8080

# 始终显示目录列表，并关闭 README 渲染
autoindex-rs ./public --no-index --no-readme

# 查看所有选项
autoindex-rs --help
```

### 从源码构建

项目要求 Rust 1.88 或更高版本，仓库提交了 `Cargo.lock`：

```bash
git clone https://github.com/kci-lnk/autoindex-rs.git
cd autoindex-rs
cargo build --locked --release
./target/release/autoindex-rs ~/Public
```

<details>
<summary><strong>English Quick Start</strong></summary>

Download the archive for your platform from [GitHub Releases](https://github.com/kci-lnk/autoindex-rs/releases/latest), verify it against `SHA256SUMS`, and run:

```bash
autoindex-rs /path/to/public
```

Open `http://127.0.0.1:6701/`. README rendering is enabled by default. CLI arguments override process environment variables, which override `.env`, which override built-in defaults.

```bash
autoindex-rs ./public --bind 127.0.0.1 --port 8080 --page-size 200
autoindex-rs ./public --no-index --no-readme
autoindex-rs --help
```

Build from source with Rust 1.88 or newer:

```bash
cargo build --locked --release
./target/release/autoindex-rs /path/to/public
```

</details>

## 下载与校验

发布包命名为 `autoindex-rs-v<版本>-<target>.tar.gz`（Unix）或 `.zip`（Windows）。每个包都包含二进制、README 和 MIT License。

| 系统 | CPU | Rust target | 格式 |
|---|---|---|---|
| Linux | x86-64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARMv7 hard-float | `armv7-unknown-linux-musleabihf` | `.tar.gz` |
| macOS | Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows | x86-64 | `x86_64-pc-windows-msvc` | `.zip` |

Linux 与 macOS：

```bash
sha256sum --ignore-missing --check SHA256SUMS  # Linux
shasum -a 256 autoindex-rs-*.tar.gz            # macOS；与 SHA256SUMS 对照
```

Windows PowerShell：

```powershell
Get-FileHash .\autoindex-rs-*.zip -Algorithm SHA256
```

## 配置

配置优先级固定为：

```text
命令行参数  >  进程环境变量  >  当前工作目录 .env  >  默认值
```

`.env` 不存在时正常启动；格式错误或参数非法时拒绝启动。

| 能力 | CLI | 环境变量 | 默认值 |
|---|---|---|---|
| 服务目录 | `[DIRECTORY]` | `AUTOINDEX_DIRECTORY` | 当前工作目录 |
| 监听 IP | `--bind <IP>` | `AUTOINDEX_BIND` | `0.0.0.0` |
| 端口 | `-p, --port <PORT>` | `AUTOINDEX_PORT` | `6701` |
| README | `--readme` / `--no-readme` | `AUTOINDEX_README` | 开启 |
| 默认文档 | 重复 `--index-file <NAME>` | `AUTOINDEX_INDEX_FILES`，逗号分隔 | `index.html,index.htm` |
| 禁用默认文档 | `--no-index` | `AUTOINDEX_INDEX_FILES=` | 关闭 index 查找 |
| 每页数量 | `--page-size <N>` | `AUTOINDEX_PAGE_SIZE` | `100`，范围 `1..=1000` |
| 显示时区 | `--timezone <IANA>` | `AUTOINDEX_TIMEZONE` | `Asia/Shanghai` |
| 日志级别 | `--log-level <LEVEL>` | `AUTOINDEX_LOG_LEVEL` | `info` |
| 解锁敏感目录 | `--allow-sensitive-paths` | `AUTOINDEX_ALLOW_SENSITIVE_PATHS` | 关闭 |

可从 [`.env.example`](.env.example) 复制配置。`--bind` 只决定本机监听地址，不参与 HTTP Host 路由。

## Index 页面与 HTTP 行为

- 目录始终排在文件之前；支持 `sort=name|size|modified`、`order=asc|desc` 和不透明双向 `cursor`。
- 默认每页 100 项，扫描上限 100 万项，游标最长 512 字节。
- 提供面包屑、父目录入口、可访问的排序控件、移动端布局，以及跟随系统并可持久化的明暗主题。
- 目录 URL 缺少尾斜杠时返回 `301`，并保留原查询参数。
- 只接受 `GET`、`HEAD`；其他方法返回 `405` 和 `Allow: GET, HEAD`。
- 文件响应包含 MIME、弱 ETag、Last-Modified、条件请求、单 Range 和 multipart Range 支持。
- 根目录不可读取时返回 `503`；不存在或不可访问的子项返回 `404`。
- 支持 SIGINT/SIGTERM，并提供最多 10 秒的优雅关停窗口。

## GitHub 风格 README

每个目录只读取自己的精确文件名 `README.md`。文件必须为 UTF-8 且不超过 1 MiB；读取或解析失败不会影响目录列表。

支持 GFM 表格、任务列表、删除线、自动链接，以及五种顶层 GitHub Alerts：

```markdown
> [!NOTE]
> Useful context.

> [!TIP]
> A practical shortcut.

> [!IMPORTANT]
> Required information.

> [!WARNING]
> A possible risk.

> [!CAUTION]
> A likely negative outcome.
```

渲染后的 HTML 会再次经过白名单清洗。脚本、事件属性、危险协议、外部图片和 data 图片会被删除；仅保留安全的相对同源图片。链接统一添加 `nofollow noopener noreferrer`，外部 HTTP(S) 链接在新窗口打开。

## 安全边界

服务根目录在启动时通过 `cap-std` 预打开，之后的目录扫描和文件打开都从该能力句柄发起。根目录内部的符号链接可以使用，但无法借此跳出根目录；解析到隐藏目标或不安全组件时同样拒绝访问。

默认隐藏或拒绝：

- 点文件、非法 UTF-8 名称、控制字符和 Unicode 格式控制字符；
- Windows 保留设备名，以及 FIFO、socket、device 等特殊文件；
- 编码分隔符、双重解码 traversal、`..` 和根级 `__*` 保留命名空间；
- 文件系统根、常见系统敏感目录、应用数据目录，以及 `~/.ssh`、`~/.gnupg`、`~/.aws`、`~/.kube`。

> [!WARNING]
> `--allow-sensitive-paths` 只解除“将敏感目录配置为服务根”的启动保护，不会增加认证或访问控制。不要把文件系统根、凭据目录或其他私密数据暴露给不可信网络。

监听地址默认是 `0.0.0.0`。只需本机访问时应显式使用 `--bind 127.0.0.1`；公网使用时建议置于具备 TLS、认证、访问控制和限流能力的反向代理之后。

## 部署

### Docker

```bash
docker build -t autoindex-rs .
docker run --rm -p 6701:6701 -v "$PWD/public:/srv:ro" autoindex-rs
```

覆盖参数时保留镜像入口点即可：

```bash
docker run --rm -p 8080:8080 -v "$PWD/public:/srv:ro" \
  autoindex-rs /srv --port 8080 --no-index
```

### systemd

仓库提供 [`examples/autoindex-rs.service`](examples/autoindex-rs.service)。复制二进制和 unit 后，根据实际目录修改 `ExecStart` 与 `ReadOnlyPaths`：

```bash
sudo install -m 0755 target/release/autoindex-rs /usr/local/bin/
sudo install -m 0644 examples/autoindex-rs.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now autoindex-rs
```

## 开发与验证

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo audit
cargo test --locked --all-features
cargo build --locked --release
./scripts/smoke.sh
```

CI 在 Linux、macOS、Windows 上运行格式、Clippy、安全审计、测试和 release build。`v*` 标签触发 Release workflow，并生成以下六个平台包：

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `armv7-unknown-linux-musleabihf`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Linux 产物由 `cross` 构建为静态 musl 二进制，并通过 QEMU 运行库测试和 HTTP 集成测试。构建后还会确认 ELF 不包含动态解释器。Release job 检查恰好生成六个压缩包，统一生成并回验 `SHA256SUMS`，最后创建 GitHub Release。

### 构建优化

发布二进制采用偏向体积的 release profile：

- `opt-level = "z"`
- fat LTO
- 单 codegen unit
- `panic = "abort"`
- 关闭调试信息并剥离符号
- Unix 使用 `gzip -9`，Windows 使用 ZIP Optimal

没有使用 UPX 等可执行文件打包器，以避免增加杀毒软件误报、启动解压和平台兼容风险。

## License

[MIT](LICENSE) © KCI-LNK

<p align="center">
  Built with Rust, Axum, Tokio, Askama, Comrak and cap-std.
</p>
