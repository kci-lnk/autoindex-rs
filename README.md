# autoindex-rs

一个独立、轻量并以文件系统边界安全为重点的目录索引 Web 服务。它提供熟悉的 `Index of /` 页面、文件下载、排序与游标分页，并能在每级目录下渲染 GitHub 风格的 `README.md`。

默认服务当前工作目录，监听 `0.0.0.0:6701`，开启 README 渲染，并优先响应 `index.html`、`index.htm`。它只服务一个目录，不包含 HTTP Host 路由、子域映射、TLS/SNI、认证、gRPC 或管理后台。

## 特性

- 单文件部署：页面模板、CSS 和 JavaScript 全部编译进二进制。
- 完整目录服务：排序、双向游标分页、面包屑、明暗主题、默认文档和 Range 下载。
- GitHub 风格 README：GFM 表格、任务列表、删除线、自动链接和五类 Alerts，并在输出前清洗 HTML。
- 能力式文件系统边界：预打开服务根目录，阻止路径穿越、隐藏文件泄露和符号链接越界。
- 六平台 Release：Linux 静态 musl、macOS Intel/Apple Silicon 和 Windows x64。

## English Quick Start

Build with Rust 1.88 or newer, then serve a directory:

```bash
cargo build --release
./target/release/autoindex-rs /path/to/public
```

Open `http://127.0.0.1:6701/`. README rendering is enabled by default. Command-line arguments override process environment variables, which override `.env`, which override built-in defaults.

```bash
autoindex-rs ./public --bind 127.0.0.1 --port 8080 --page-size 200
autoindex-rs ./public --no-index --no-readme
autoindex-rs --help
```

Tagged releases contain the binary, this README, and the MIT license. Choose the archive matching your platform from GitHub Releases and verify it against `SHA256SUMS` before running it.

## 安装与运行

项目要求 Rust 1.88 或更高版本，仓库提交了 `Cargo.lock`：

```bash
git clone https://github.com/kci-lnk/autoindex-rs.git
cd autoindex-rs
cargo build --locked --release
./target/release/autoindex-rs ~/Public
```

也可以从带 `v*` 标签的 GitHub Release 下载对应平台压缩包，使用 `SHA256SUMS` 校验后直接运行。发布包命名为 `autoindex-rs-v<版本>-<target>.tar.gz`（Unix）或 `.zip`（Windows）。

| 系统 | CPU | Rust target | 发布格式 |
|---|---|---|---|
| Linux | x86-64 | `x86_64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARM64 | `aarch64-unknown-linux-musl` | `.tar.gz` |
| Linux | ARMv7 hard-float | `armv7-unknown-linux-musleabihf` | `.tar.gz` |
| macOS | Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows | x86-64 | `x86_64-pc-windows-msvc` | `.zip` |

Linux 与 macOS 可在下载目录校验所有已下载的包：

```bash
sha256sum --ignore-missing --check SHA256SUMS  # Linux
shasum -a 256 autoindex-rs-*.tar.gz            # macOS；与 SHA256SUMS 对照
```

Windows PowerShell：

```powershell
Get-FileHash .\autoindex-rs-*.zip -Algorithm SHA256
```

## 配置

配置优先级固定为：命令行 > 进程环境变量 > 启动时当前工作目录中的 `.env` > 默认值。`.env` 不存在时正常启动；格式错误或参数非法时拒绝启动。

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
- 页面模板、CSS 和主题脚本通过 Askama 编译进二进制，不需要运行时静态资源目录。

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

默认还会隐藏或拒绝：

- 点文件、非法 UTF-8 名称、控制字符和 Unicode 格式控制字符；
- Windows 保留设备名，以及 FIFO、socket、device 等特殊文件；
- 编码分隔符、双重解码 traversal、`..` 和根级 `__*` 保留命名空间；
- 文件系统根、常见系统敏感目录、应用数据目录，以及 `~/.ssh`、`~/.gnupg`、`~/.aws`、`~/.kube`。

`--allow-sensitive-paths` 只解除“配置为敏感根目录”的启动保护，不会解除 URL 路径校验、隐藏文件规则、特殊文件过滤或符号链接隔离。监听默认是全网卡；只需本机访问时应显式使用 `--bind 127.0.0.1`，公网使用时建议置于具备 TLS、访问控制和限流能力的反向代理之后。

## Docker

```bash
docker build -t autoindex-rs .
docker run --rm -p 6701:6701 -v "$PWD/public:/srv:ro" autoindex-rs
```

覆盖参数时保留镜像的入口点即可：

```bash
docker run --rm -p 8080:8080 -v "$PWD/public:/srv:ro" \
  autoindex-rs /srv --port 8080 --no-index
```

## systemd

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

CI 会在 Linux、macOS、Windows 运行检查。`v*` 标签的 Release workflow 构建：

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `armv7-unknown-linux-musleabihf`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Linux 产物由 `cross` 构建为静态 musl 二进制并在 QEMU 下运行目标测试；Unix 发布 `.tar.gz`，Windows 发布 `.zip`，所有包包含二进制、README 和 MIT License。

### Release 构建与发布

`.github/workflows/release.yml` 只由 `v*` 标签触发。标签必须严格等于 `v` 加 `Cargo.toml` 中的版本，例如当前版本对应 `v0.1.0`；不匹配会在构建前失败。发布前先确认主分支 CI 通过，然后创建并推送带注释标签：

```bash
git tag -a v0.1.0 -m "autoindex-rs v0.1.0"
git push origin v0.1.0
```

Release job 会测试并构建六个 target，验证 Linux ELF 没有动态解释器，检查恰好生成六个压缩包，统一生成并回验 `SHA256SUMS`，最后创建 GitHub Release。只有最终发布 job 具备 `contents: write` 权限，其余 job 保持只读。

发布二进制使用偏向体积的 Cargo release profile：`opt-level = "z"`、fat LTO、单 codegen unit、`panic = "abort"`、关闭调试信息并剥离符号。Unix 包额外使用 `gzip -9`，Windows 使用 ZIP Optimal 压缩。没有使用 UPX 等可执行文件打包器，避免增加杀毒软件误报、启动解压和平台兼容风险。

## License

MIT © KCI-LNK。详见 [LICENSE](LICENSE)。
