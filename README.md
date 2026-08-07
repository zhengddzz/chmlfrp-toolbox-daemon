# ChmlFrp 社区工具箱 Daemon

> 服务器端远程管理守护进程，配合 [ChmlFrp 社区工具箱](https://github.com/zhengddzz/ChmlFrp-Community-Toolbox) 桌面客户端使用。

## 简介

Daemon 是 ChmlFrp 社区工具箱的服务器端组件，部署在 Linux 服务器上后，可被同账号的桌面客户端远程管理：

- 端到端延迟测试（ICMP Ping + TCP Ping）
- 端到端带宽测试（HTTP 下载/上传测速）
- 多租户支持（一个 Daemon 可同时被多个账号绑定）

## 系统要求

- **架构**：x64 (x86_64) 或 ARM64 (aarch64)
- **系统**：Ubuntu 22.04+ / Debian 12+ / Raspberry Pi OS (Bookworm) / Armbian
- **不支持**：32 位 ARM (armv7) 设备（如树莓派 3 及更早版本）

## 安装

### 方式一：一键安装脚本（推荐）

```bash
curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash
```

安装脚本会自动完成以下操作：
1. 检测系统架构（x64 / ARM64）
2. 创建专用系统用户 `chmlfrp-daemon`
3. 从 GitHub Releases 下载并安装 deb 包
4. 生成配置文件 `/etc/chmlfrp-toolbox-daemon/config.toml`
5. 部署管理菜单到 `/usr/local/bin/chmlfrp-toolbox`
6. 交互式引导配置（后端地址、proxyToken、设备名称）
7. 启动 systemd 服务并设置开机自启

**引导配置流程：**
1. 确认后端地址（默认 `wss://api.cct.zdzz.top`）
2. 输入 proxyToken（从桌面客户端获取）
3. 输入设备名称（默认使用主机名）
4. 询问是否立即启动服务

### 方式二：管理菜单（独立脚本）

安装完成后，随时可运行管理菜单修改配置。管理菜单是一个独立脚本，类似 `./nezha.sh` 的使用方式：

```bash
# 已安装时直接运行（推荐）
sudo chmlfrp-toolbox

# 未安装时也可通过 curl 直接运行（会自动引导安装）
curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh | sudo bash
```

菜单功能：

```
=== ChmlFrp 工具箱 Daemon 管理菜单 ===

  服务状态: 运行中    账号数量: 2

  1) 查看配置
  2) 添加账号
  3) 修改账号
  4) 删除账号
  5) 修改后端地址
  6) 测试连接
  7) 启动服务
  8) 停止服务
  9) 重启服务
 10) 查看运行状态
 11) 查看日志
 12) 卸载
 13) 检查更新
  0) 退出
```

- **测试连接**：自动验证后端连通性和每个 Token 有效性
- **查看日志**：显示最近 50 行日志，可选实时跟踪
- **多租户**：通过「添加账号」可配置多个账号
- **未安装自动引导**：检测到未安装时会提示一键安装
- **检查更新**：支持检查管理菜单脚本和 Daemon 二进制的最新版本，并提供一键更新

> 管理菜单脚本由后端托管，地址：`https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh`

### 方式三：手动安装 deb 包

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

# 单账号
[[accounts]]
proxy_token = "你的_proxyToken"
device_name = "西安服务器"

# 多租户：添加多个 [[accounts]] 即可
[[accounts]]
proxy_token = "另一个用户的_proxyToken"
device_name = "西安服务器-用户B"
```

> 推荐使用 `sudo chmlfrp-toolbox` 打开管理菜单进行配置，无需手动编辑文件。

## 启动

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
# 方式一：通过管理菜单卸载（推荐）
sudo chmlfrp-toolbox
# 选择 12) 卸载

# 方式二：使用安装脚本卸载
curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash -s -- --uninstall

# 方式三：手动卸载
sudo systemctl stop chmlfrp-toolbox-daemon
sudo systemctl disable chmlfrp-toolbox-daemon
sudo apt remove chmlfrp-toolbox-daemon
sudo rm -f /usr/local/bin/chmlfrp-toolbox
```

## 目录结构

| 路径 | 说明 |
|------|------|
| `/usr/bin/chmlfrp-toolbox-daemon` | 二进制文件 |
| `/usr/local/bin/chmlfrp-toolbox` | 管理菜单脚本（独立命令，类似 nezha.sh） |
| `/etc/chmlfrp-toolbox-daemon/config.toml` | 配置文件 |
| `/etc/systemd/system/chmlfrp-toolbox-daemon.service` | systemd 服务文件 |
| `/var/lib/chmlfrp-toolbox-daemon/` | 数据目录（device_id、SQLite 数据库） |

## 多租户说明

Daemon 支持多租户：一台服务器可同时被多个 qzhua 账号绑定。

- 每个 `[[accounts]]` 配置一个 `proxy_token`，建立独立 WebSocket 连接
- 共享同一个 `device_id`，但数据按 `user_id` 隔离
- 数据存储在 `/var/lib/chmlfrp-toolbox-daemon/users/<user_id>.db`
- 用户通过桌面客户端「删除设备数据」时，仅删除该 user_id 的数据

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
- Daemon 恒为「可远程管理」状态（interconnect=1）

## 开源声明

本工具为社区开源项目，与 ChmlFrp 官方无隶属关系。

## License

MIT
