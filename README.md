# ChmlFrp 社区工具箱 Daemon

> 服务器端远程管理守护进程，配合 [ChmlFrp 社区工具箱](https://github.com/zhengddzz/ChmlFrp-Community-Toolbox) 桌面客户端使用。

## 简介

Daemon 是 ChmlFrp 社区工具箱的服务器端组件，部署在 Linux 服务器上后，可被同账号的桌面客户端远程管理：

- 端到端延迟测试（ICMP Ping + TCP Ping）
- 端到端带宽测试（HTTP 下载/上传测速）
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

### 手动安装 deb 包

```bash
# x64
sudo dpkg -i chmlfrp-toolbox-daemon_amd64.deb
sudo apt-get install -f  # 修复依赖

# ARM64
sudo dpkg -i chmlfrp-toolbox-daemon_arm64.deb
sudo apt-get install -f  # 修复依赖
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

## 开源声明

本工具为社区开源项目，与 ChmlFrp 官方无隶属关系。

## License

MIT
