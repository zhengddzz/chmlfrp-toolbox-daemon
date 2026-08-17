# ChmlFrp 社区工具箱 Daemon

> 服务器端远程管理守护进程，配合 [ChmlFrp 社区工具箱](https://github.com/zhengddzz/ChmlFrp-Community-Toolbox) 桌面客户端使用。

## 简介

Daemon 是 ChmlFrp 社区工具箱的服务器端组件，部署在 Linux 服务器上后，可被同账号的桌面客户端远程管理：

- 端到端全链路延迟测试（4 次隧道 RTT 探测、抖动和丢包率）
- 端到端固定时长带宽测试（默认 15 秒、逐秒速度采样和实时进度）
- 远程配置管理（账号增删改、后端地址修改）
- 远程服务控制（启动/停止/重启/状态查询）
- 远程日志查看（journalctl）
- 在线检查并安装更新
- 多租户支持（一个 Daemon 可同时被多个账号绑定）

## 系统要求

- **架构**：x64 (x86_64) 或 ARM64 (aarch64)
- **系统**：Ubuntu 20.04+ / Debian 11+ / CentOS 8+ / Raspberry Pi OS (Bookworm) / Armbian
- **不支持**：32 位 ARM (armv7) 设备（如树莓派 3 及更早版本）

## 安装

### 一键安装（推荐）

```bash
curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash
```

安装脚本会自动完成以下操作：
1. 检查 root 权限
2. 检测系统架构（x64 / ARM64）
3. 创建专用系统用户 `chmlfrp-daemon`（容器/受限环境自动降级为 root 运行）
4. 从 GitHub Releases 下载并安装 deb 包
5. 生成配置文件 `/etc/chmlfrp-toolbox-daemon/config.toml`
6. 创建数据目录 `/var/lib/chmlfrp-toolbox-daemon`
7. 确保 systemd service 文件存在（缺失时自动创建）
8. 交互式引导配置（后端地址、proxyToken、设备名称）
9. 启动 systemd 服务并设置开机自启

**引导配置流程：**
1. 确认后端地址（默认 `wss://api.cct.zdzz.top`）
2. 输入 proxyToken（从桌面客户端获取）
3. 输入设备名称（默认使用主机名）
4. 询问是否立即启动服务

### 手动安装 deb 包（Ubuntu / Debian）

```bash
# x64
sudo dpkg -i chmlfrp-toolbox-daemon_amd64.deb
sudo apt-get install -f  # 修复依赖

# ARM64
sudo dpkg -i chmlfrp-toolbox-daemon_arm64.deb
sudo apt-get install -f  # 修复依赖
```

### 手动安装 rpm 包（CentOS / RHEL / Fedora）

```bash
# x64
sudo dnf install -y chmlfrp-toolbox-daemon_x64.rpm
# 或 CentOS 7
sudo yum install -y chmlfrp-toolbox-daemon_x64.rpm

# ARM64
sudo dnf install -y chmlfrp-toolbox-daemon_arm64.rpm
# 或 CentOS 7
sudo yum install -y chmlfrp-toolbox-daemon_arm64.rpm
```

手动安装后需自行编辑配置文件：

```bash
sudo nano /etc/chmlfrp-toolbox-daemon/config.toml
sudo systemctl start chmlfrp-toolbox-daemon
```

## 配置

配置文件路径：`/etc/chmlfrp-toolbox-daemon/config.toml`

```toml
[server]
backend_url = "wss://api.cct.zdzz.top"
data_dir = "/var/lib/chmlfrp-toolbox-daemon"

[update]
auto_update = false

# 单账号
[[accounts]]
proxy_token = "你的_proxyToken"
device_name = "西安服务器"

# 多租户：添加多个 [[accounts]] 即可
[[accounts]]
proxy_token = "另一个用户的_proxyToken"
device_name = "西安服务器-用户B"
```

> 推荐通过桌面客户端「设备管理」页面远程修改配置，无需手动编辑文件。

## 远程管理

安装完成后，通过桌面客户端的「设备管理」页面远程管理此 Daemon：

- **配置管理**：查看/修改后端地址、添加/修改/删除账号
- **服务控制**：启动、停止、重启服务，查看运行状态
- **日志查看**：查看最近的运行日志
- **检查更新**：打开更新管理时自动通过 `u.zdzz.top` 获取最新版本，也可手动重新检查并一键安装
- **自动更新**：可配置开关，开启后由后端推送触发自动更新

设备名称以账号设备记录中的自定义名称为准，Daemon 重连时会携带配置中的名称用于首次注册，后续重连不会覆盖已保存名称。重装或卸载时请保留 `/var/lib/chmlfrp-toolbox-daemon/device_id`，否则会生成新的设备 ID，并被识别为新设备。

服务端作为测速发送端时，Daemon 会按 `ip`、`realIp`、`real_IP` 顺序解析节点地址并跳过空值；节点接口返回鉴权失败、节点不存在等错误时，客户端会显示接口返回的具体原因。

Daemon 会在首次需要建立测速隧道时自动准备 frpc：优先复用 `/usr/local/bin/frpc`、`/usr/bin/frpc` 等已有安装；系统未安装时，从与桌面客户端相同的 frpc 下载接口选择当前 Linux 架构版本，完成大小和 SHA-256 校验后保存到 `{data_dir}/bin/frpc`（默认 `/var/lib/chmlfrp-toolbox-daemon/bin/frpc`）。下载或校验失败不会留下可执行的半成品文件。

创建临时隧道遇到“远程端口已被占用”时，Daemon 会在节点允许的端口范围内自动更换不重复端口，最多尝试 5 次；鉴权失败、限流、节点异常等非端口冲突错误不会盲目重试。

Daemon 启动临时 frpc 前会通过 ChmlFrp 用户信息接口获取 `usertoken` 并写入受限权限的配置文件。启动后持续检查 frpc 进程状态，并从远端节点端口发送 `PING`、等待对应 `PONG`；只有真实转发链路在 30 秒内就绪才向发起端返回地址。frpc 提前退出时会返回脱敏后的启动日志，超时则自动清理进程、临时隧道和本地测速服务。

## 服务管理

```bash
# 启动服务
sudo systemctl start chmlfrp-toolbox-daemon

# 设置开机自启（安装时已自动设置）
sudo systemctl enable chmlfrp-toolbox-daemon

# 查看运行状态
sudo systemctl status chmlfrp-toolbox-daemon

# 查看日志
sudo journalctl -u chmlfrp-toolbox-daemon -f
```

## CLI 命令

```bash
# 前台运行（调试用）
chmlfrp-toolbox-daemon start

# 查看状态
chmlfrp-toolbox-daemon status

# 生成配置模板
chmlfrp-toolbox-daemon init-config -o /path/to/config.toml

# 指定配置文件
chmlfrp-toolbox-daemon --config /path/to/config.toml start
```

## 卸载

```bash
# 使用安装脚本卸载
curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash -s -- --uninstall

# 或本地执行
sudo bash install.sh --uninstall

# 手动卸载
sudo systemctl stop chmlfrp-toolbox-daemon
sudo systemctl disable chmlfrp-toolbox-daemon
sudo apt remove chmlfrp-toolbox-daemon
sudo rm -rf /etc/chmlfrp-toolbox-daemon /var/lib/chmlfrp-toolbox-daemon
```

## 目录结构

| 路径 | 说明 |
|------|------|
| `/usr/bin/chmlfrp-toolbox-daemon` | 二进制文件 |
| `/etc/chmlfrp-toolbox-daemon/config.toml` | 配置文件 |
| `/etc/systemd/system/chmlfrp-toolbox-daemon.service` | systemd 服务文件 |
| `/var/lib/chmlfrp-toolbox-daemon/` | 数据目录（device_id、SQLite 数据库） |

## 多租户说明

Daemon 支持多租户：一台服务器可同时被多个 qzhua 账号绑定。

- 每个 `[[accounts]]` 配置一个 `proxy_token`，建立独立 WebSocket 连接
- 共享同一个 `device_id`，但数据按 `user_id` 隔离
- 数据存储在 `/var/lib/chmlfrp-toolbox-daemon/users/<user_id>.db`
- 用户通过桌面客户端「删除设备数据」时，仅删除该 user_id 的数据

## 令牌与授权机制

daemon 配置中的 `proxy_token` 为后端代理令牌（7 天有效期），**不能**直接调用 chmlfrp API（cf-v2.uapis.cn 只认 qzhua accessToken，直接用会报「无效的登录状态」）。

### accessToken 自动刷新（v0.3.11+）

与桌面客户端同款流程：调用 chmlfrp API（测速隧道创建/删除等）前，daemon 自动用 `proxy_token` 调后端 `POST /auth/refresh` 换取 accessToken：

- 内存缓存，剩余有效期 > 60 秒直接复用，不重复请求（后端限流 5 次/分钟）
- 并发调用单飞合并，避免重复刷新
- 刷新失败按错误码分类提示：令牌过期 → 引导重新授权；限流 → 稍后重试

### 远程重新授权（v0.3.11+）

`proxy_token` 临近过期时，在桌面客户端「设备管理 → 远程管理 → 重新授权」操作：

1. 桌面端打开浏览器完成 qzhua OAuth 授权，获取新 `proxy_token`
2. 通过 relay RPC（`update_proxy_token` 命令）把新令牌发给 daemon
3. daemon 校验新令牌（调 /auth/refresh）→ 按旧令牌定位并更新配置文件 → 清空 accessToken 缓存
4. daemon 主动断开 relay 连接，用新令牌自动重连（约 3-5 秒），无需重启服务

> 注意：设备已离线（令牌彻底过期导致断连）时无法远程更新，需在服务器上手动修改 `/etc/chmlfrp-toolbox-daemon/config.toml` 中的 `proxy_token` 后重启服务。

## 容器/受限环境适配

安装脚本内置三级降级策略，确保在大多数环境下可正常安装：

1. **容器检测**：通过 `/.dockerenv`、`/proc/1/cgroup`、`systemd-detect-virt` 识别 Docker/LXC/Podman/OpenVZ 等容器环境，直接以 root 运行
2. **命令缺失检测**：系统缺少 `groupadd`/`useradd` 命令时降级 root
3. **创建失败降级**：`groupadd`/`useradd` 执行失败（如权限受限）时降级 root，不中断安装

降级为 root 后，脚本会自动修补 service 文件的 `User=`/`Group=` 行，确保 systemd 服务能以 root 启动。

## 远程更新机制

Daemon 服务运行在 systemd 沙箱中（`ProtectSystem=strict` + `PrivateTmp` + `ReadWritePaths=/var/lib/chmlfrp-toolbox-daemon /etc/chmlfrp-toolbox-daemon`），远程更新的执行链路专门适配了该沙箱：

1. **互斥**：自动更新与手动更新同时触发时，后触发的请求直接被拒绝，避免重复下载与包管理器锁争抢
2. **下载**：更新包下载到数据目录 `updates/`（沙箱内可写且对外可见的路径之一；`PrivateTmp` 使 daemon 的 `/tmp` 对沙箱外进程不可见）。只接受与当前架构（x64/arm64）和包格式（deb/rpm）严格匹配的安装包，拒绝跨架构、跨发行版回退
3. **安全更新助手**（v0.3.16+，推荐）：通过 `systemd-run --wait --pipe --quiet` 在系统 manager 中启动 transient unit 执行 `/usr/lib/chmlfrp-toolbox-daemon/secure-update-helper.sh`。助手由 root 管理，校验包路径（仅限数据目录 `updates/`）、固定文件名、SHA-256、包名与包架构，并复制到 root 暂存区二次校验后再安装（防 TOCTOU）。**不能**直接 `sudo dpkg`：sudo 提权后的子进程仍处于服务的只读 mount namespace，dpkg 写 `/var/lib/dpkg` 会报 `Read-only file system`
4. **回退链**：旧环境未部署助手时回退 `dpkg -i` / `rpm -U` 直装；dpkg 失败 → `apt-get install -f` 修复依赖 → `dpkg-deb -x` 解包直接替换二进制（适配容器等无 dpkg 数据库环境）
5. **重启**：先尝试 `systemctl daemon-reload`（使新安装的 service 文件生效）再 `systemctl restart`（D-Bus 通信，不受文件系统沙箱影响）

配置目录 `/etc/chmlfrp-toolbox-daemon`（目录 770、文件 660、属组 daemon）在 `ReadWritePaths` 中放行，供远程修改配置（重新授权、账号管理、后端地址）直接写入 `config.toml`。

sudoers 免密规则由安装脚本生成（`/etc/sudoers.d/chmlfrp-toolbox-daemon`），v0.3.16+ 仅放行安全更新助手与 systemctl 服务控制/journalctl 命令（不再直接放行 dpkg/rpm），规则参数顺序与 daemon 源码 `build_escalated_cmd` 逐字对应。

### 常见更新故障排查

| 现象 | 原因与处理 |
|------|-----------|
| `dpkg: unable to access the dpkg database directory /var/lib/dpkg: Read-only file system` | 旧版本（≤ v0.3.9）在沙箱内直接执行 dpkg 所致。在服务器上重新运行一键安装脚本升级到新版即可，后续更新走 systemd-run 不再复现 |
| `sudo: a password is required` / `sudo: no tty present` | sudoers 规则缺失或参数不匹配（重新运行安装脚本），或 service 文件被改为 `NoNewPrivileges=true`（安装脚本会自动修正为 false） |
| 日志提示"未找到安全更新助手，回退 dpkg/rpm 直装模式" | 服务器上的安装脚本版本较旧（≤ v0.3.15），未部署助手。重新运行一键安装脚本即可启用安全更新模式 |
| `Running in chroot, ignoring request` / systemd-run 报 D-Bus 错误 | 无 systemd 的容器环境。daemon 会自动回退为直接执行 dpkg；若 dpkg 数据库同样只读，再降级为解包替换二进制 |

## 从源码构建

```bash
# 安装 Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 克隆仓库
git clone https://github.com/zhengddzz/chmlfrp-toolbox-daemon.git
cd chmlfrp-toolbox-daemon

# 构建
cargo build --release

# 构建 deb 包（需安装 cargo-deb）
cargo install cargo-deb
cargo deb
```

## 与桌面客户端的关系

- 桌面客户端通过 WebSocket 连接后端中继服务
- 同账号下的所有设备（桌面客户端 + Daemon）自动互相发现
- 桌面客户端可对 Daemon 执行远程延迟/带宽测试
- 桌面客户端可远程管理 Daemon 的配置、服务、日志和更新
- Daemon 恒为「可远程管理」状态（interconnect=1）

## 测速协议

- 统一使用 `SPEEDTEST_TIME <duration_ms>`，支持 5～120 秒固定时长测速。
- `tcp_speed_test` RPC 必须提供 `durationSeconds`，不接受固定大小参数。
- 连接 3 秒内没有首包时直接返回失败，不尝试旧协议回退。
- 测速从首包开始计时，到达目标时长后两端主动关闭连接，不等待 EOF 或长写超时。
- 结果始终返回本次测试的 `speedSamples`。
- Mbps 使用十进制公式 `bytes × 8 ÷ seconds ÷ 1,000,000`，有效时长从首批数据到达后开始计算。

## 开源声明

本工具为社区开源项目，与 ChmlFrp 官方无隶属关系。

## License

MIT
