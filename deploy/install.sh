#!/bin/bash
#=============================================================
# ChmlFrp 社区工具箱 Daemon 一键安装脚本
#
# 功能：
#   1. 自动识别包管理器（apt/yum/dnf），下载并安装 Daemon（deb 或 rpm 包）
#   2. 安装后自动引导用户配置 proxyToken
#   3. 兼容容器/受限环境（自动降级为 root 运行）
#
# 支持：
#   - 架构：x64 (x86_64)、ARM64 (aarch64)
#   - 系统：Ubuntu / Debian / CentOS / RHEL / Fedora / Raspberry Pi OS / Armbian
#
# 用法：
#   sudo bash install.sh                    # 安装（安装后自动引导配置）
#   sudo bash install.sh --local <path>     # 使用本地 deb/rpm 包安装
#   sudo bash install.sh --uninstall        # 卸载
#
# 在线一键安装：
#   curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash
#
# 安装后管理：通过桌面客户端设备管理页面远程管理
#=============================================================

# 不使用 set -e：安装脚本需要在部分步骤（如创建用户）失败时降级处理，
# 而非直接退出。关键步骤通过显式 `|| error` 处理。

# ===== 颜色定义 =====
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ===== 默认配置 =====
APP_NAME="chmlfrp-toolbox-daemon"
APP_USER="chmlfrp-daemon"
APP_GROUP="chmlfrp-daemon"
INSTALL_DIR="/usr/bin"
CONFIG_DIR="/etc/chmlfrp-toolbox-daemon"
CONFIG_FILE="${CONFIG_DIR}/config.toml"
DATA_DIR="/var/lib/chmlfrp-toolbox-daemon"
SERVICE_FILE="/etc/systemd/system/${APP_NAME}.service"
GITHUB_REPO="zhengddzz/chmlfrp-toolbox-daemon"
UPDATE_API="https://u.zdzz.top/api/toolbox-daemon"
DEFAULT_BACKEND_URL="wss://api.cct.zdzz.top"

# ===== 输出函数（输出到 stderr，避免干扰函数返回值）=====
info()    { echo -e "${BLUE}[INFO]${NC} $1" >&2; }
success() { echo -e "${GREEN}[OK]${NC} $1" >&2; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1" >&2; }
error()   { echo -e "${RED}[ERROR]${NC} $1" >&2; exit 1; }
title()   { echo -e "\n${BOLD}${CYAN}=== $1 ===${NC}\n" >&2; }

# ===== 基础检查 =====
check_root() {
    if [[ $EUID -ne 0 ]]; then
        error "请使用 root 权限运行此脚本（sudo bash install.sh）"
    fi
}

detect_arch() {
    local arch
    arch=$(uname -m)
    case "$arch" in
        x86_64)  echo "x64" ;;
        aarch64) echo "arm64" ;;
        *)       error "不支持的架构: $arch（仅支持 x64 和 ARM64）" ;;
    esac
}

detect_pkg_manager() {
    if command -v apt-get &>/dev/null; then
        echo "apt"
    elif command -v yum &>/dev/null; then
        echo "yum"
    elif command -v dnf &>/dev/null; then
        echo "dnf"
    else
        echo "unknown"
    fi
}

# 检测是否运行在容器/受限环境中（此时 groupadd/useradd 通常不可用或被限制）
is_container_env() {
    # Docker 容器标识
    [[ -f /.dockerenv ]] && return 0
    # cgroup 标识（兼容 cgroup v1/v2）
    if [[ -f /proc/1/cgroup ]]; then
        grep -qaE 'lxc|docker|containerd|kubepods' /proc/1/cgroup 2>/dev/null && return 0
    fi
    # systemd-detect-virt（多数现代发行版自带）
    if command -v systemd-detect-virt &>/dev/null; then
        local virt
        virt=$(systemd-detect-virt 2>/dev/null || echo "")
        case "$virt" in
            lxc|docker|containerd|podman|openvz|systemd-nspawn) return 0 ;;
        esac
    fi
    return 1
}

# ===== 用户/目录管理 =====
create_user() {
    # 容器/受限环境：直接以 root 运行，跳过用户创建
    if is_container_env; then
        warn "检测到容器/受限环境，将以 root 运行（跳过用户创建）"
        APP_USER="root"
        APP_GROUP="root"
        return 0
    fi

    # 命令存在性检查
    if ! command -v groupadd &>/dev/null || ! command -v useradd &>/dev/null; then
        warn "系统缺少 groupadd/useradd 命令，将以 root 运行"
        APP_USER="root"
        APP_GROUP="root"
        return 0
    fi

    # 创建用户组
    if ! getent group "$APP_GROUP" &>/dev/null; then
        if groupadd --system "$APP_GROUP" 2>/dev/null; then
            info "创建用户组: $APP_GROUP"
        else
            warn "创建用户组失败（权限受限？），将以 root 运行"
            APP_USER="root"
            APP_GROUP="root"
            return 0
        fi
    fi

    # 创建用户
    if ! id "$APP_USER" &>/dev/null; then
        # nologin 路径兼容（部分系统在 /sbin/nologin，部分在 /usr/sbin/nologin）
        local nologin_shell="/usr/sbin/nologin"
        [[ -x /sbin/nologin ]] && nologin_shell="/sbin/nologin"
        if useradd --system --no-create-home --shell "$nologin_shell" --gid "$APP_GROUP" "$APP_USER" 2>/dev/null; then
            info "创建用户: $APP_USER"
        else
            warn "创建用户失败（权限受限？），将以 root 运行"
            APP_USER="root"
            APP_GROUP="root"
            return 0
        fi
    fi
}

# 部署安全更新助手：root 管理的固定提权入口，
# 由助手自身校验更新包路径/文件名/SHA-256/包名/包架构后再安装，
# 避免旧的 "sudoers 直接放行 dpkg/rpm + 数据目录可写" 提权风险
install_secure_helper() {
    local helper_dir="/usr/lib/${APP_NAME}"
    local helper_file="${helper_dir}/secure-update-helper.sh"

    if ! mkdir -p "$helper_dir" 2>/dev/null; then
        warn "无法创建 $helper_dir，远程更新将回退直装模式"
        return 0
    fi

    # 写入脚本内容（与 deploy/secure-update-helper.sh 保持一致）
    if ! cat > "$helper_file" << 'HELPER_EOF'
#!/bin/sh
# chmlfrp-toolbox-daemon 安全更新助手（仅限 root 执行）
#
# 作用：作为 sudoers 唯一放行的提权入口，替代 daemon 直接调用 dpkg/rpm。
# 安全校验：
#   1. 仅接受固定更新目录、固定文件名的 deb/rpm 包
#   2. 拒绝符号链接与路径越界
#   3. SHA-256 校验（调用方传入期望值）
#   4. 复制到 root 管理的暂存区后二次校验，防止校验后被替换（TOCTOU）
#   5. 校验包名与包架构必须匹配当前系统
#
# 用法: secure-update-helper.sh <包文件> <期望sha256>
set -eu

ALLOWED_DIR="/var/lib/chmlfrp-toolbox-daemon/updates"
PKG_NAME="chmlfrp-toolbox-daemon"

log() { printf '[secure-update-helper] %s\n' "$*" >&2; }
die() { log "错误: $*"; exit 1; }

[ "$(id -u)" -eq 0 ] || die "必须以 root 运行"
[ "$#" -eq 2 ] || die "参数格式: $0 <包文件> <sha256>"

pkg_file=$1
expected_sha=$2

# 1. 路径校验：必须是固定更新目录下的绝对路径
case "$pkg_file" in
  "$ALLOWED_DIR"/*) ;;
  *) die "包文件必须位于 $ALLOWED_DIR" ;;
esac

# 2. 存在性校验 + 拒绝符号链接（真实路径也必须仍在允许目录内）
[ -f "$pkg_file" ] || die "包文件不存在"
real_path=$(readlink -f "$pkg_file" 2>/dev/null || true)
[ -n "$real_path" ] || die "无法解析文件真实路径"
case "$real_path" in
  "$ALLOWED_DIR"/*) ;;
  *) die "文件真实路径越界: $real_path" ;;
esac
[ "$real_path" = "$pkg_file" ] || die "拒绝符号链接路径"

# 3. 文件名固定，杜绝执行任意文件
base=$(basename "$real_path")
case "$base" in
  "${PKG_NAME}_update.deb"|"${PKG_NAME}_update.rpm") ;;
  *) die "非法文件名: $base" ;;
esac

# 4. 原文件 SHA-256 校验
if command -v sha256sum >/dev/null 2>&1; then
  actual_sha=$(sha256sum "$real_path" | awk '{print $1}')
else
  die "系统缺少 sha256sum 工具"
fi
[ "$actual_sha" = "$expected_sha" ] || die "SHA-256 校验失败: 期望 $expected_sha，实际 $actual_sha"

# 5. 复制到 root 管理的暂存区，防止校验后原文件被 daemon 用户替换
stage_dir=$(mktemp -d /tmp/chmlfrp-update.XXXXXX) || die "创建暂存目录失败"
trap 'rm -rf "$stage_dir"' EXIT
staged="$stage_dir/$base"
cp -- "$real_path" "$staged" || die "复制包到暂存区失败"
chown root:root "$staged"
chmod 0644 "$staged"

# 6. 暂存副本二次 SHA-256 校验
staged_sha=$(sha256sum "$staged" | awk '{print $1}')
[ "$staged_sha" = "$expected_sha" ] || die "暂存副本 SHA-256 校验失败"

# 7. 包名与包架构校验
arch=$(uname -m)
case "$base" in
  *.deb)
    command -v dpkg-deb >/dev/null 2>&1 || die "系统缺少 dpkg-deb 工具"
    pkg_arch=$(dpkg-deb -f "$staged" Architecture 2>/dev/null || true)
    pkg_id=$(dpkg-deb -f "$staged" Package 2>/dev/null || true)
    case "$arch:$pkg_arch" in
      x86_64:amd64|aarch64:arm64) ;;
      *) die "deb 包架构不匹配: 当前 $arch，包为 ${pkg_arch:-未知}" ;;
    esac
    [ "$pkg_id" = "$PKG_NAME" ] || die "deb 包名不匹配: ${pkg_id:-未知}"
    ;;
  *.rpm)
    command -v rpm >/dev/null 2>&1 || die "系统缺少 rpm 工具"
    pkg_arch=$(rpm -qp --queryformat '%{ARCH}' "$staged" 2>/dev/null || true)
    pkg_id=$(rpm -qp --queryformat '%{NAME}' "$staged" 2>/dev/null || true)
    case "$arch:$pkg_arch" in
      x86_64:x86_64|aarch64:aarch64) ;;
      *) die "rpm 包架构不匹配: 当前 $arch，包为 ${pkg_arch:-未知}" ;;
    esac
    [ "$pkg_id" = "$PKG_NAME" ] || die "rpm 包名不匹配: ${pkg_id:-未知}"
    ;;
esac

# 8. 安装暂存副本（deb 依赖缺失时自动修复）
case "$base" in
  *.deb)
    if ! dpkg -i -- "$staged"; then
      apt-get install -f -y || die "dpkg 安装失败且依赖修复失败"
    fi
    ;;
  *.rpm)
    rpm -U --force -- "$staged" || die "rpm 安装失败"
    ;;
esac

log "安全更新完成: $base"
HELPER_EOF
    then
        warn "无法写入 $helper_file，远程更新将回退直装模式"
        return 0
    fi

    chown root:root "$helper_file"
    chmod 0755 "$helper_file"
    success "安全更新助手已部署: $helper_file"
}

# 配置 sudoers：仅放行安全更新助手与服务控制命令（v0.3.16+ 收紧）
# 旧的 dpkg/rpm/apt-get/install 直装规则已移除：
# 数据目录归 daemon 用户可写，若同时放行从该目录安装软件包的命令，
# 任何取得 daemon 用户权限的进程都能以 root 身份安装任意软件包（提权）。
setup_sudoers() {
    # root 模式不需要 sudoers
    if [[ "$APP_USER" == "root" ]]; then
        return 0
    fi

    local sudoers_file="/etc/sudoers.d/${APP_NAME}"
    info "配置 sudoers 规则（仅放行安全更新助手与服务控制）..."

    cat > "$sudoers_file" << SUDOERS_EOF
# ${APP_NAME} - 最小权限 sudoers（v0.3.16+）
# 由安装脚本自动生成，请勿手动修改

# ===== 安全更新助手（远程更新唯一提权入口）=====
# 服务沙箱 ProtectSystem=strict 使 sudo 提权后的子进程仍处于只读
# mount namespace，dpkg 直写 /var/lib/dpkg 会报 Read-only file system。
# systemd-run 在系统 manager 中启动 transient unit，于沙箱外执行助手。
# 助手自身校验包路径/文件名/SHA-256/包名/包架构后安装，
# 通配符仅匹配助手之后的参数（包路径与哈希），不可执行任意命令。
# 注意：参数顺序与 daemon 源码 build_escalated_cmd 逐字对应，勿调整。
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemd-run --wait --pipe --quiet /usr/lib/${APP_NAME}/secure-update-helper.sh *

# ===== systemctl 服务控制（start/stop/restart/daemon-reload）=====
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl daemon-reload
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl start ${APP_NAME}
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl start ${APP_NAME}.service
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl stop ${APP_NAME}
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl stop ${APP_NAME}.service
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl restart ${APP_NAME}
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/systemctl restart ${APP_NAME}.service

# ===== journalctl 查看日志 =====
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/journalctl -u ${APP_NAME} *
${APP_USER} ALL=(root) NOPASSWD: /usr/bin/journalctl -u ${APP_NAME}.service *
SUDOERS_EOF

    chmod 440 "$sudoers_file" 2>/dev/null || {
        warn "sudoers 文件权限设置失败，远程更新可能无法正常工作"
        return 0
    }

    # 验证 sudoers 语法（visudo -c）
    if command -v visudo &>/dev/null; then
        if ! visudo -cf "$sudoers_file" &>/dev/null; then
            warn "sudoers 语法错误，删除规则文件"
            rm -f "$sudoers_file"
            return 0
        fi
    fi

    success "sudoers 规则已配置"
}

setup_journal_access() {
    if [[ "$APP_USER" == "root" ]] || ! getent group systemd-journal &>/dev/null; then
        return 0
    fi
    usermod -aG systemd-journal "$APP_USER" 2>/dev/null || warn "无法将 ${APP_USER} 加入 systemd-journal 组"
}

# 确保 service 文件存在：不存在时从模板创建，降级 root 时修补 User/Group
# 容器/受限环境下 /etc/systemd/system 可能不可写，降级为提示手动管理
ensure_service_file() {
    # service 文件不存在（deb 包未包含或安装失败），从模板创建
    if [[ ! -f "$SERVICE_FILE" ]]; then
        info "service 文件不存在，正在创建..."

        # 尝试写入 service 文件
        if ! cat > "$SERVICE_FILE" 2>/dev/null << EOF
[Unit]
Description=ChmlFrp Community Toolbox Daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${APP_USER}
Group=${APP_GROUP}
SupplementaryGroups=systemd-journal
ExecStart=${INSTALL_DIR}/${APP_NAME} --config ${CONFIG_FILE} start
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

# NoNewPrivileges 必须为 false：更新流程依赖 sudo 提权，
# true 会禁用 setuid 导致 sudo 失效（远程更新无法安装）
NoNewPrivileges=false
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR} ${CONFIG_DIR}
PrivateTmp=true

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF
        then
            warn "无法写入 $SERVICE_FILE（权限受限）"
            info "service 文件需手动创建，或直接运行: $INSTALL_DIR/$APP_NAME --config $CONFIG_FILE start"
            return 0
        fi
        chmod 644 "$SERVICE_FILE" 2>/dev/null || true
        success "service 文件已创建: $SERVICE_FILE"
        return 0
    fi

    # 文件已存在：降级到 root 时修补 User=/Group= 行和 ExecStart 路径
    if [[ "$APP_USER" == "root" ]] || [[ "$INSTALL_DIR" != "/usr/bin" ]]; then
        local changed=false
        if grep -q '^User=' "$SERVICE_FILE"; then
            sed -i 's|^User=.*|User=root|' "$SERVICE_FILE" 2>/dev/null && changed=true
        fi
        if grep -q '^Group=' "$SERVICE_FILE"; then
            sed -i 's|^Group=.*|Group=root|' "$SERVICE_FILE" 2>/dev/null && changed=true
        fi
        # 修正 ExecStart 路径（手动安装可能改到了 /usr/local/bin）
        if grep -q '^ExecStart=' "$SERVICE_FILE"; then
            sed -i "s|^ExecStart=.*|ExecStart=${INSTALL_DIR}/${APP_NAME} --config ${CONFIG_FILE} start|" "$SERVICE_FILE" 2>/dev/null && changed=true
        fi
        if $changed; then
            info "已调整 service 配置（运行用户: root，路径: $INSTALL_DIR）"
        fi
    fi

    # 升级旧版配置：NoNewPrivileges=true 会导致 sudo 提权失效（远程更新无法安装）
    if grep -q '^NoNewPrivileges=true' "$SERVICE_FILE" 2>/dev/null; then
        sed -i 's|^NoNewPrivileges=true|NoNewPrivileges=false|' "$SERVICE_FILE" 2>/dev/null
        if command -v systemctl &>/dev/null; then
            systemctl daemon-reload 2>/dev/null || true
        fi
        info "已修正 service 配置: NoNewPrivileges=false（否则 sudo 无法提权）"
    fi

    # 升级旧版配置：ReadWritePaths 未包含配置目录时，远程修改配置会报
    # "Read-only file system"（ProtectSystem=strict 使 /etc 整体只读，
    # 目录 770/文件 660 的 DAC 权限无法越过挂载层只读）
    if grep -q '^ReadWritePaths=' "$SERVICE_FILE" 2>/dev/null; then
        if ! grep -qE "^ReadWritePaths=.*${CONFIG_DIR}" "$SERVICE_FILE" 2>/dev/null; then
            sed -i "s|^ReadWritePaths=.*|ReadWritePaths=${DATA_DIR} ${CONFIG_DIR}|" "$SERVICE_FILE" 2>/dev/null
            if command -v systemctl &>/dev/null; then
                systemctl daemon-reload 2>/dev/null || true
                systemctl restart "$APP_NAME" 2>/dev/null || true
            fi
            info "已修正 service 配置: ReadWritePaths 增加 ${CONFIG_DIR}（远程配置写入需要）"
        fi
    fi
}

create_data_dir() {
    mkdir -p "${DATA_DIR}/users"
    chown -R "$APP_USER":"$APP_GROUP" "$DATA_DIR"
    chmod 700 "$DATA_DIR"
}

# ===== 下载 =====
download_file() {
    local url=$1
    local output=$2
    if command -v curl &>/dev/null; then
        if [[ "$output" == "-" ]]; then
            curl -fsSL "$url"
        else
            curl -fsSL -o "$output" "$url"
        fi
    elif command -v wget &>/dev/null; then
        if [[ "$output" == "-" ]]; then
            wget -q -O - "$url"
        else
            wget -q -O "$output" "$url"
        fi
    else
        error "需要 curl 或 wget 来下载文件"
    fi
}

# 从更新 API 的 JSON 中提取 linux 平台指定 arch 和 format 的下载 URL
# 优先使用 jq，其次 python3，最后 python2
get_package_url() {
    local json=$1
    local arch=$2
    local fmt=$3

    if command -v jq &>/dev/null; then
        echo "$json" | jq -r ".platforms.linux[] | select(.arch==\"$arch\" and .format==\"$fmt\") | .url" 2>/dev/null | head -1
    elif command -v python3 &>/dev/null; then
        echo "$json" | python3 -c "
import json,sys
try:
    data=json.load(sys.stdin)
    for p in data.get('platforms',{}).get('linux',[]):
        if p.get('arch')=='$arch' and p.get('format')=='$fmt':
            print(p['url']); break
except: pass
" 2>/dev/null
    elif command -v python &>/dev/null; then
        echo "$json" | python -c "
import json,sys
try:
    data=json.load(sys.stdin)
    for p in data.get('platforms',{}).get('linux',[]):
        if p.get('arch')=='$arch' and p.get('format')=='$fmt':
            print(p['url']); break
except: pass
" 2>/dev/null
    else
        error "需要 jq 或 python 来解析版本信息，请安装后重试"
    fi
}

# 获取最新版本号
get_latest_version() {
    local json=$1
    if command -v jq &>/dev/null; then
        echo "$json" | jq -r '.version' 2>/dev/null
    elif command -v python3 &>/dev/null; then
        echo "$json" | python3 -c "import json,sys; print(json.load(sys.stdin).get('version',''))" 2>/dev/null
    elif command -v python &>/dev/null; then
        echo "$json" | python -c "import json,sys; print(json.load(sys.stdin).get('version',''))" 2>/dev/null
    fi
}

# 生成 UUID（兼容多种系统）
generate_uuid() {
    if [[ -r /proc/sys/kernel/random/uuid ]]; then
        cat /proc/sys/kernel/random/uuid
    elif command -v uuidgen &>/dev/null; then
        uuidgen | tr 'A-Z' 'a-z'
    elif command -v openssl &>/dev/null; then
        openssl rand -hex 16
    else
        error "无法生成 UUID，请安装 uuidgen 或 openssl"
    fi
}

# 从 WebSocket URL 获取 HTTP API URL（wss→https, ws→http）
get_api_base_url() {
    local ws_url
    ws_url=$(config_get_backend_url)
    echo "$ws_url" | sed 's|^wss://|https://|; s|^ws://|http://|'
}

# 从 JSON 字符串中提取指定键的值（单层键）
json_get() {
    local json=$1
    local key=$2
    if command -v jq &>/dev/null; then
        echo "$json" | jq -r ".$key" 2>/dev/null
    elif command -v python3 &>/dev/null; then
        echo "$json" | python3 -c "import json,sys; d=json.load(sys.stdin); v=d.get('$key'); print(v if v is not None else '')" 2>/dev/null
    elif command -v python &>/dev/null; then
        echo "$json" | python -c "import json,sys; d=json.load(sys.stdin); v=d.get('$key'); print(v if v is not None else '')" 2>/dev/null
    fi
}

download_package() {
    local arch=$1
    local pkg_type=$2  # deb | rpm

    info "正在获取最新版本信息..."
    local api_data
    api_data=$(download_file "$UPDATE_API" "-" 2>/dev/null || echo "")

    if [[ -z "$api_data" ]]; then
        error "无法获取版本信息，请检查网络连接"
    fi

    local version
    version=$(get_latest_version "$api_data")
    if [[ -n "$version" ]]; then
        info "最新版本: v$version"
    fi

    local download_url
    download_url=$(get_package_url "$api_data" "$arch" "$pkg_type")

    if [[ -z "$download_url" ]]; then
        error "未找到架构 ${arch} 的 ${pkg_type} 安装包"
    fi

    # 下载到 /tmp 固定路径（避免子shell trap 删除临时目录导致文件丢失）
    local pkg_file="/tmp/${APP_NAME}_install.${pkg_type}"
    info "正在下载: $download_url"
    download_file "$download_url" "$pkg_file" || error "下载 ${pkg_type} 包失败"
    echo "$pkg_file"
}

# ===== 安装 =====

# 手动解压 deb 包并安装二进制（容器/受限环境下 dpkg 失败时的降级方案）
# service 文件由 ensure_service_file 自动创建，不依赖 deb 包
install_deb_manual() {
    local deb_file=$1
    local tmp_extract
    tmp_extract=$(mktemp -d)
    trap "rm -rf $tmp_extract" RETURN

    info "尝试手动解压 deb 包安装..."
    if ! dpkg-deb -x "$deb_file" "$tmp_extract" 2>/dev/null; then
        error "解压 deb 包失败，请检查文件是否完整"
    fi

    # 查找二进制文件并复制
    local bin_src="${tmp_extract}/usr/bin/${APP_NAME}"
    if [[ ! -f "$bin_src" ]]; then
        error "deb 包中未找到二进制文件 usr/bin/${APP_NAME}"
    fi

    cp "$bin_src" "$INSTALL_DIR/$APP_NAME" 2>/dev/null || {
        # /usr/bin 可能也受限，尝试 /usr/local/bin
        INSTALL_DIR="/usr/local/bin"
        cp "$bin_src" "$INSTALL_DIR/$APP_NAME" || error "无法复制二进制到 $INSTALL_DIR"
    }
    chmod 755 "$INSTALL_DIR/$APP_NAME"

    # 更新 service 文件中的 ExecStart 路径（若 INSTALL_DIR 变化）
    if [[ "$INSTALL_DIR" != "/usr/bin" ]]; then
        info "二进制安装到: $INSTALL_DIR/$APP_NAME"
    fi

    success "手动安装完成"
}

install_deb() {
    local deb_file=$1
    info "正在安装..."
    if dpkg -i "$deb_file" 2>/dev/null; then
        # dpkg 成功，验证二进制
        if [[ -x "$INSTALL_DIR/$APP_NAME" ]]; then
            success "安装完成"
            return 0
        fi
    fi

    # dpkg 失败或二进制不存在，尝试自动修复依赖
    warn "dpkg 安装失败（可能是容器/受限环境），尝试自动修复..."
    if apt-get install -f -y 2>/dev/null && [[ -x "$INSTALL_DIR/$APP_NAME" ]]; then
        success "依赖修复后安装完成"
        return 0
    fi

    # 仍然失败，降级为手动解压安装
    install_deb_manual "$deb_file"
}

# 手动解压 rpm 包并安装二进制（容器/受限环境下 rpm 失败时的降级方案）
install_rpm_manual() {
    local rpm_file=$1
    local tmp_extract
    tmp_extract=$(mktemp -d)
    trap "rm -rf $tmp_extract" RETURN

    info "尝试手动解压 rpm 包安装..."
    if command -v rpm2cpio &>/dev/null && command -v cpio &>/dev/null; then
        rpm2cpio "$rpm_file" | cpio -idmv -D "$tmp_extract" 2>/dev/null || error "解压 rpm 包失败"
    else
        error "解压 rpm 包需要 rpm2cpio 和 cpio，请安装后重试"
    fi

    # 查找二进制文件并复制
    local bin_src="${tmp_extract}/usr/bin/${APP_NAME}"
    if [[ ! -f "$bin_src" ]]; then
        # 尝试在解压目录中搜索
        bin_src=$(find "$tmp_extract" -name "$APP_NAME" -type f 2>/dev/null | head -1)
        if [[ -z "$bin_src" ]]; then
            error "rpm 包中未找到二进制文件 ${APP_NAME}"
        fi
    fi

    cp "$bin_src" "$INSTALL_DIR/$APP_NAME" 2>/dev/null || {
        INSTALL_DIR="/usr/local/bin"
        cp "$bin_src" "$INSTALL_DIR/$APP_NAME" || error "无法复制二进制到 $INSTALL_DIR"
    }
    chmod 755 "$INSTALL_DIR/$APP_NAME"

    if [[ "$INSTALL_DIR" != "/usr/bin" ]]; then
        info "二进制安装到: $INSTALL_DIR/$APP_NAME"
    fi

    success "手动安装完成"
}

install_rpm() {
    local rpm_file=$1
    info "正在安装..."
    if command -v dnf &>/dev/null; then
        if dnf install -y "$rpm_file" 2>/dev/null && [[ -x "$INSTALL_DIR/$APP_NAME" ]]; then
            success "安装完成"
            return 0
        fi
    elif command -v yum &>/dev/null; then
        if yum install -y "$rpm_file" 2>/dev/null && [[ -x "$INSTALL_DIR/$APP_NAME" ]]; then
            success "安装完成"
            return 0
        fi
    else
        error "无法找到 RPM 包管理器（dnf/yum）"
    fi

    # rpm 安装失败，降级为手动解压
    warn "rpm 安装失败（可能是容器/受限环境）"
    install_rpm_manual "$rpm_file"
}

# ===== 配置文件读写函数 =====

config_get_backend_url() {
    if [[ ! -f "$CONFIG_FILE" ]]; then
        echo "$DEFAULT_BACKEND_URL"
        return
    fi
    local url
    url=$(grep -E '^\s*backend_url\s*=' "$CONFIG_FILE" | head -1 | sed -E 's/.*=\s*"([^"]*)".*/\1/')
    echo "${url:-$DEFAULT_BACKEND_URL}"
}

config_set_backend_url() {
    local new_url=$1
    if grep -q '^\s*backend_url\s*=' "$CONFIG_FILE"; then
        sed -i "s|^\(\s*backend_url\s*=\s*\).*|\1\"${new_url}\"|" "$CONFIG_FILE"
    fi
}

config_count_accounts() {
    if [[ ! -f "$CONFIG_FILE" ]]; then
        echo 0
        return
    fi
    grep -c '^\s*\[\[accounts\]\]' "$CONFIG_FILE" 2>/dev/null || echo 0
}

config_get_account_token() {
    local n=$1
    local tokens
    tokens=$(grep -E '^\s*proxy_token\s*=' "$CONFIG_FILE" | sed -E 's/.*=\s*"([^"]*)".*/\1/')
    echo "$tokens" | sed -n "${n}p"
}

config_get_account_name() {
    local n=$1
    local names
    names=$(grep -E '^\s*device_name\s*=' "$CONFIG_FILE" | sed -E 's/.*=\s*"([^"]*)".*/\1/')
    echo "$names" | sed -n "${n}p"
}

config_exists() {
    [[ -f "$CONFIG_FILE" ]]
}

config_is_configured() {
    if ! config_exists; then
        return 1
    fi
    if grep -q "在此填入" "$CONFIG_FILE"; then
        return 1
    fi
    # 必须存在至少一个非空 proxy_token 才算已配置
    local token
    token=$(grep -E '^\s*proxy_token\s*=' "$CONFIG_FILE" | head -1 | sed -E 's/.*=\s*"([^"]*)".*/\1/')
    [[ -n "$token" ]]
}

generate_config() {
    if [[ ! -f "$CONFIG_FILE" ]]; then
        mkdir -p "$CONFIG_DIR"
        chmod 770 "$CONFIG_DIR"
        chown root:"$APP_GROUP" "$CONFIG_DIR"
        cat > "$CONFIG_FILE" << EOF
# ChmlFrp 工具箱 Daemon 配置文件
# 由 install.sh 生成于 $(date '+%Y-%m-%d %H:%M:%S')

[server]
backend_url = "${DEFAULT_BACKEND_URL}"
data_dir = "${DATA_DIR}"

# 账号列表（可配置多个，实现多租户）
# 通过安装引导或桌面客户端添加，格式：
# [[accounts]]
# proxy_token = "你的proxyToken"
# device_name = "设备名称"
EOF
        chmod 660 "$CONFIG_FILE"
        chown root:"$APP_GROUP" "$CONFIG_FILE"
        success "配置文件已生成: $CONFIG_FILE"
    else
        info "配置文件已存在，跳过生成"
        # 修复旧版权限：确保目录和配置文件可被 daemon 用户组写入
        chmod 770 "$CONFIG_DIR" 2>/dev/null || true
        chown root:"$APP_GROUP" "$CONFIG_DIR" 2>/dev/null || true
        chmod 660 "$CONFIG_FILE" 2>/dev/null || true
        chown root:"$APP_GROUP" "$CONFIG_FILE" 2>/dev/null || true
    fi
}

config_add_account() {
    local token=$1
    local name=$2
    if [[ -f "$CONFIG_FILE" ]] && [[ -s "$CONFIG_FILE" ]]; then
        if [[ "$(tail -c1 "$CONFIG_FILE")" != "" ]]; then
            echo "" >> "$CONFIG_FILE"
        fi
    fi
    cat >> "$CONFIG_FILE" << EOF
[[accounts]]
proxy_token = "${token}"
device_name = "${name}"
EOF
}

# ===== 服务管理 =====
service_start() {
    ensure_service_file

    # 检测 systemd 是否可用（容器环境可能未运行 systemd）
    if ! command -v systemctl &>/dev/null || [[ ! -d /run/systemd/system ]]; then
        warn "systemd 不可用（可能是容器环境），改用后台方式启动"
        service_start_nohup
        return $?
    fi

    systemctl daemon-reload
    systemctl enable "$APP_NAME" 2>/dev/null || true

    if ! config_is_configured; then
        warn "配置尚未完成，请完成配置后运行: sudo systemctl start $APP_NAME"
        success "已设置开机自启"
        return 0
    fi

    # 判断服务是否已在运行：运行中用 restart，未运行用 start
    local action="start"
    if systemctl is-active --quiet "$APP_NAME" 2>/dev/null; then
        action="restart"
    fi

    # 尝试通过 systemctl 启动/重启（root 在非交互 shell 下可能被 polkit 拦截）
    local start_err
    start_err=$(systemctl "$action" "$APP_NAME" 2>&1)
    if [[ $? -eq 0 ]] && systemctl is-active --quiet "$APP_NAME"; then
        success "服务已${action}"
        success "已设置开机自启"
        return 0
    fi

    # systemctl 失败：判断是否为 polkit 交互认证拦截
    if echo "$start_err" | grep -qiE "Interactive authentication required|Access denied|insufficient privilege"; then
        warn "systemctl 被拦截（需要交互认证），改用 systemd-run 启动..."
        # 方案一：systemd-run 启动 transient 服务单元
        if command -v systemd-run &>/dev/null; then
            if systemd-run --no-block --unit="${APP_NAME}-run" \
                --description="ChmlFrp Toolbox Daemon (transient)" \
                --property=Restart=always \
                --property=RestartSec=5 \
                "$INSTALL_DIR/$APP_NAME" --config "$CONFIG_FILE" start 2>&1 | grep -v "^$"; then
                sleep 1
                if systemctl is-active --quiet "${APP_NAME}-run"; then
                    success "服务已通过 systemd-run 启动"
                    warn "注：使用临时单元 ${APP_NAME}-run，重启后需用 systemctl start $APP_NAME 启动"
                    success "已设置开机自启"
                    return 0
                fi
            fi
        fi
    fi

    # 降级方案：nohup 后台运行（会先停止旧进程）
    warn "systemd ${action} 失败：$start_err"
    warn "降级为后台进程方式启动..."
    service_start_nohup
    success "已设置开机自启"
}

# 后台进程方式启动（不依赖 systemd 服务管理）
service_start_nohup() {
    if ! config_is_configured; then
        warn "配置尚未完成，无法启动"
        return 1
    fi

    # 停止旧进程
    pkill -f "$INSTALL_DIR/$APP_NAME" 2>/dev/null || true
    sleep 1

    # 启动新进程
    local log_file="${DATA_DIR}/daemon.log"
    mkdir -p "$DATA_DIR"
    chown "$APP_USER":"$APP_GROUP" "$DATA_DIR" 2>/dev/null || true

    if [[ "$APP_USER" == "root" ]]; then
        nohup "$INSTALL_DIR/$APP_NAME" --config "$CONFIG_FILE" start >> "$log_file" 2>&1 &
    else
        nohup su -s /bin/bash "$APP_USER" -c \
            "\"$INSTALL_DIR/$APP_NAME\" --config \"$CONFIG_FILE\" start" >> "$log_file" 2>&1 &
    fi

    local pid=$!
    sleep 2

    if kill -0 "$pid" 2>/dev/null; then
        success "服务已后台启动 (PID: $pid)"
        info "日志文件: $log_file"
        info "停止命令: kill $pid"
        return 0
    else
        error "后台启动失败，请检查日志: $log_file"
    fi
}

# ===== 交互式配置引导 =====

guided_config() {
    title "配置引导"

    echo "欢迎使用 ChmlFrp 社区工具箱 Daemon！"
    echo ""

    # 配置账号
    local count
    count=$(config_count_accounts)
    if [[ "$count" -gt 0 ]] && config_is_configured; then
        echo "已检测到 $count 个已配置账号"
        echo ""
        read -p "是否添加新账号？[y/N] " -r < /dev/tty
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            add_account_interactive
        fi
    else
        echo "尚未配置任何账号，现在开始配置第一个账号。"
        echo ""
        add_account_interactive
    fi

    # 完成配置
    title "配置完成"
    echo "当前配置摘要："
    echo "  账号数量: $(config_count_accounts)"
    echo ""

    read -p "是否立即启动服务？[Y/n] " -r < /dev/tty
    if [[ ! $REPLY =~ ^[Nn]$ ]]; then
        service_start
    fi
}

add_account_interactive() {
    echo ""
    echo "--- 添加账号 ---"
    echo ""

    # 输入设备名称
    local default_name
    default_name=$(hostname 2>/dev/null || echo "服务器")
    read -p "设备名称 [${default_name}]: " -r name < /dev/tty
    name="${name:-$default_name}"

    # 生成授权 session
    local api_base
    api_base=$(get_api_base_url)
    local session_id
    session_id=$(generate_uuid)
    local auth_url="${api_base}/auth/login?session=${session_id}"

    # 预创建 pending 会话（用户打开链接时也会自动创建/更新）
    info "正在创建授权会话..."
    download_file "$auth_url" "-" >/dev/null 2>&1 || true

    # 显示授权链接
    echo ""
    echo "请在浏览器中打开以下链接完成授权登录："
    echo ""
    echo -e "  ${CYAN}${BOLD}${auth_url}${NC}"
    echo ""
    warn "授权有效期：5分钟，请尽快完成"
    echo ""

    # 轮询授权状态
    info "等待授权完成（请勿关闭此终端）..."
    local elapsed=0
    local max_wait=300
    local poll_interval=3
    while [[ $elapsed -lt $max_wait ]]; do
        local remaining=$((max_wait - elapsed))
        printf "\r剩余时间: %02d:%02d  " $((remaining / 60)) $((remaining % 60))

        sleep $poll_interval
        elapsed=$((elapsed + poll_interval))

        local status_data
        status_data=$(download_file "${api_base}/auth/status?session=${session_id}" "-" 2>/dev/null || echo "")

        if [[ -z "$status_data" ]]; then
            continue
        fi

        local status
        status=$(json_get "$status_data" "status")

        case "$status" in
            completed)
                local proxy_token
                proxy_token=$(json_get "$status_data" "proxyToken")
                if [[ -n "$proxy_token" ]] && [[ "$proxy_token" != "null" ]]; then
                    echo ""
                    success "授权成功"
                    config_add_account "$proxy_token" "$name"
                    success "账号已添加: $name"
                    return 0
                fi
                ;;
            failed)
                echo ""
                error "授权失败，请重新运行安装"
                ;;
            not_found|pending)
                # 等待用户完成授权
                ;;
        esac
    done

    echo ""
    error "授权超时（5分钟内未完成），请重新运行安装"
}

# ===== 卸载 =====
uninstall() {
    title "卸载"
    info "开始卸载 $APP_NAME..."

    # 1. 停止所有运行形态：正式 unit、transient unit、nohup 后台进程
    systemctl stop "$APP_NAME" 2>/dev/null || true
    systemctl stop "${APP_NAME}-run" 2>/dev/null || true
    pkill -f "$INSTALL_DIR/$APP_NAME" 2>/dev/null || true
    systemctl disable "$APP_NAME" 2>/dev/null || true

    # 2. 优先通过包管理器卸载（保持包数据库一致，便于后续升级/重装）
    #    手动安装（脚本未走包管理器）或包管理器卸载失败时回退手动清理
    if command -v dpkg &>/dev/null && dpkg -s "$APP_NAME" &>/dev/null; then
        if dpkg -r "$APP_NAME" &>/dev/null; then
            success "已通过 dpkg 卸载软件包"
        else
            warn "dpkg 卸载失败，回退手动清理"
        fi
    elif command -v rpm &>/dev/null && rpm -q "$APP_NAME" &>/dev/null; then
        if rpm -e "$APP_NAME" &>/dev/null; then
            success "已通过 rpm 卸载软件包"
        else
            warn "rpm 卸载失败，回退手动清理"
        fi
    fi

    # 3. 手动清理二进制与 unit（包管理器已卸载时此步为幂等兜底）
    rm -f "$INSTALL_DIR/$APP_NAME"
    rm -f "/usr/local/bin/$APP_NAME"
    rm -f "$SERVICE_FILE"
    rm -f "/etc/sudoers.d/${APP_NAME}"
    rm -f "/usr/lib/${APP_NAME}/secure-update-helper.sh"
    rmdir "/usr/lib/${APP_NAME}" 2>/dev/null || true

    # 4. 配置目录含 proxyToken 等敏感信息，删除前单独确认
    if [[ -d "$CONFIG_DIR" ]]; then
        read -p "是否删除配置目录 $CONFIG_DIR（含 proxyToken 等账号信息）？[y/N] " -r < /dev/tty
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            rm -rf "$CONFIG_DIR"
            success "配置目录已删除"
        else
            info "配置目录已保留: $CONFIG_DIR"
        fi
    fi

    # 仅在非 root 降级模式下清理用户/组
    if [[ "$APP_USER" != "root" ]] && id "$APP_USER" &>/dev/null; then
        userdel "$APP_USER" 2>/dev/null || true
    fi
    if [[ "$APP_GROUP" != "root" ]] && getent group "$APP_GROUP" &>/dev/null; then
        groupdel "$APP_GROUP" 2>/dev/null || true
    fi

    systemctl daemon-reload 2>/dev/null || true

    # 5. 数据目录含 device_id，保留可在重装后继续关联原设备
    if [[ -d "$DATA_DIR" ]]; then
        read -p "是否删除数据目录 $DATA_DIR（含 device_id，保留可关联原设备）？[y/N] " -r < /dev/tty
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            rm -rf "$DATA_DIR"
            success "数据目录已删除"
        else
            info "数据目录已保留: $DATA_DIR"
        fi
    fi

    success "卸载完成"
}

# ===== 主函数 =====
main() {
    local local_file=""
    local do_uninstall=false

    # 解析参数
    while [[ $# -gt 0 ]]; do
        case $1 in
            --local)
                local_file="$2"
                shift 2
                ;;
            --uninstall)
                do_uninstall=true
                shift
                ;;
            --help|-h)
                echo "用法: sudo bash install.sh [选项]"
                echo ""
                echo "选项:"
                echo "  (无参数)          安装并引导配置"
                echo "  --local <path>    使用本地 deb/rpm 包安装"
                echo "  --uninstall       卸载 Daemon"
                echo "  --help            显示帮助"
                echo ""
                echo "在线一键安装："
                echo "  curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash"
                exit 0
                ;;
            *)
                error "未知参数: $1"
                ;;
        esac
    done

    check_root

    # 脚本退出时清理临时下载文件
    trap "rm -f /tmp/${APP_NAME}_install.deb /tmp/${APP_NAME}_install.rpm" EXIT

    # 卸载模式
    if $do_uninstall; then
        uninstall
        exit 0
    fi

    # ===== 安装流程 =====
    title "ChmlFrp 社区工具箱 Daemon 安装程序"

    local arch
    arch=$(detect_arch)
    info "检测到架构: $arch"

    local pkg_manager
    pkg_manager=$(detect_pkg_manager)
    info "包管理器: $pkg_manager"

    create_user

    if [[ -n "$local_file" ]]; then
        if [[ ! -f "$local_file" ]]; then
            error "文件不存在: $local_file"
        fi
        info "使用本地文件: $local_file"
        case "$local_file" in
            *.deb)  install_deb "$local_file" ;;
            *.rpm)  install_rpm "$local_file" ;;
            *)      error "不支持的文件格式（仅支持 .deb 和 .rpm）" ;;
        esac
    else
        case "$pkg_manager" in
            apt)
                local deb_file
                deb_file=$(download_package "$arch" "deb")
                install_deb "$deb_file"
                ;;
            yum|dnf)
                local rpm_file
                rpm_file=$(download_package "$arch" "rpm")
                install_rpm "$rpm_file"
                ;;
            *)
                error "不支持的包管理器: $pkg_manager（仅支持 apt/yum/dnf）"
                ;;
        esac
    fi

    generate_config
    create_data_dir
    ensure_service_file
    setup_journal_access
    install_secure_helper
    setup_sudoers

    # 安装后自动引导配置
    if ! config_is_configured; then
        guided_config
    else
        info "已检测到有效配置，跳过引导"
        service_start
    fi

    # 安装完成提示
    echo ""
    success "=== 安装完成 ==="
    echo ""
    info "运行用户: $APP_USER"
    info "管理方式："
    echo "  通过桌面客户端的「设备管理」页面远程管理此 Daemon"
    echo "  支持配置管理、服务控制、检查更新、查看日志"
    echo ""
    info "或直接编辑配置文件："
    echo "  sudo nano $CONFIG_FILE"
    echo ""
    info "常用命令："
    echo "  sudo systemctl start $APP_NAME    # 启动服务"
    echo "  sudo systemctl status $APP_NAME   # 查看状态"
    echo "  sudo journalctl -u $APP_NAME -f   # 查看日志"
}

main "$@"
