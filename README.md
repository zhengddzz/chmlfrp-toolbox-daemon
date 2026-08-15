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
- **检查更新**：检查 GitHub Releases 是否有新版本，并一键安装
- **自动更新**：可配置开关，开启后由后端推送触发自动更新

服务端作为测速发送端时，Daemon 会按 `ip`、`realIp`、`real_IP` 顺序解析节点地址并跳过空值；节点接口返回鉴权失败、节点不存在等错误时，客户端会显示接口返回的具体原因。

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

## 容器/受限环境适配

安装脚本内置三级降级策略，确保在大多数环境下可正常安装：

1. **容器检测**：通过 `/.dockerenv`、`/proc/1/cgroup`、`systemd-detect-virt` 识别 Docker/LXC/Podman/OpenVZ 等容器环境，直接以 root 运行
2. **命令缺失检测**：系统缺少 `groupadd`/`useradd` 命令时降级 root
3. **创建失败降级**：`groupadd`/`useradd` 执行失败（如权限受限）时降级 root，不中断安装

降级为 root 后，脚本会自动修补 service 文件的 `User=`/`Group=` 行，确保 systemd 服务能以 root 启动。

## 远程更新机制

Daemon 服务运行在 systemd 沙箱中（`ProtectSystem=strict` + `PrivateTmp` + `ReadWritePaths=/var/lib/chmlfrp-toolbox-daemon`），远程更新的执行链路专门适配了该沙箱：

1. **下载**：更新包下载到数据目录 `updates/`（沙箱内唯一可写且对外可见的路径；`PrivateTmp` 使 daemon 的 `/tmp` 对沙箱外进程不可见）
2. **安装**：通过 `systemd-run --wait --pipe --quiet` 在系统 manager 中启动 transient unit 执行 `dpkg -i` / `rpm -U`。**不能**直接 `sudo dpkg`：sudo 提权后的子进程仍处于服务的只读 mount namespace，dpkg 写 `/var/lib/dpkg` 会报 `Read-only file system`
3. **降级链**：dpkg 失败 → `apt-get install -f` 修复依赖 → `dpkg-deb -x` 解包直接替换二进制（适配容器等无 dpkg 数据库环境）
4. **重启**：`systemctl restart`（D-Bus 通信，不受文件系统沙箱影响）

sudoers 免密规则由安装脚本生成（`/etc/sudoers.d/chmlfrp-toolbox-daemon`），规则参数顺序与 daemon 源码 `build_escalated_cmd` 逐字对应。

### 常见更新故障排查

| 现象 | 原因与处理 |
|------|-----------|
| `dpkg: unable to access the dpkg database directory /var/lib/dpkg: Read-only file system` | 旧版本（≤ v0.3.9）在沙箱内直接执行 dpkg 所致。在服务器上重新运行一键安装脚本升级到新版即可，后续更新走 systemd-run 不再复现 |
| `sudo: a password is required` / `sudo: no tty present` | sudoers 规则缺失或参数不匹配（重新运行安装脚本），或 service 文件被改为 `NoNewPrivileges=true`（安装脚本会自动修正为 false） |
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
