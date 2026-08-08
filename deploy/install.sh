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

# 确保 service 文件存在：不存在时从模板创建，降级 root 时修补 User/Group
ensure_service_file() {
    # service 文件不存在（deb 包未包含或安装失败），从模板创建
    if [[ ! -f "$SERVICE_FILE" ]]; then
        info "service 文件不存在，正在创建..."
        cat > "$SERVICE_FILE" << EOF
[Unit]
Description=ChmlFrp Community Toolbox Daemon
Description[zh_CN]=ChmlFrp 社区工具箱 Daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${APP_USER}
Group=${APP_GROUP}
ExecStart=${INSTALL_DIR}/${APP_NAME} --config ${CONFIG_FILE} start
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR}
PrivateTmp=true

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF
        chmod 644 "$SERVICE_FILE"
        success "service 文件已创建: $SERVICE_FILE"
        return 0
    fi

    # 文件已存在：降级到 root 时修补 User=/Group= 行
    if [[ "$APP_USER" == "root" ]]; then
        local changed=false
        if grep -q '^User=' "$SERVICE_FILE"; then
            sed -i 's|^User=.*|User=root|' "$SERVICE_FILE"
            changed=true
        fi
        if grep -q '^Group=' "$SERVICE_FILE"; then
            sed -i 's|^Group=.*|Group=root|' "$SERVICE_FILE"
            changed=true
        fi
        if $changed; then
            info "已将 service 运行用户调整为 root（适配受限环境）"
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
install_deb() {
    local deb_file=$1
    info "正在安装..."
    if ! dpkg -i "$deb_file"; then
        warn "依赖缺失或安装失败，尝试自动修复..."
        apt-get install -f -y || error "依赖安装失败，请手动运行: apt-get install -f"
    fi
    # 验证二进制是否安装成功
    if [[ ! -x "$INSTALL_DIR/$APP_NAME" ]]; then
        error "安装失败：未找到 $INSTALL_DIR/$APP_NAME，请检查 deb 包是否完整"
    fi
    success "安装完成"
}

install_rpm() {
    local rpm_file=$1
    info "正在安装..."
    if command -v dnf &>/dev/null; then
        dnf install -y "$rpm_file" || error "RPM 安装失败"
    elif command -v yum &>/dev/null; then
        yum install -y "$rpm_file" || error "RPM 安装失败"
    else
        error "无法找到 RPM 包管理器（dnf/yum）"
    fi
    # 验证二进制是否安装成功
    if [[ ! -x "$INSTALL_DIR/$APP_NAME" ]]; then
        error "安装失败：未找到 $INSTALL_DIR/$APP_NAME，请检查 rpm 包是否完整"
    fi
    success "安装完成"
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
        chmod 640 "$CONFIG_FILE"
        chown root:"$APP_GROUP" "$CONFIG_FILE"
        success "配置文件已生成: $CONFIG_FILE"
    else
        info "配置文件已存在，跳过生成"
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
        warn "systemd 不可用（可能是容器环境），无法通过 systemctl 管理服务"
        info "可手动运行: $INSTALL_DIR/$APP_NAME --config $CONFIG_FILE start"
        return 0
    fi

    systemctl daemon-reload
    systemctl enable "$APP_NAME" 2>/dev/null || true
    if config_is_configured; then
        if systemctl start "$APP_NAME"; then
            success "服务已启动"
        else
            warn "服务启动失败，请检查日志: journalctl -u $APP_NAME -e"
        fi
    else
        warn "配置尚未完成，请完成配置后运行: sudo systemctl start $APP_NAME"
    fi
    success "已设置开机自启"
}

# ===== 交互式配置引导 =====

guided_config() {
    title "配置引导"

    echo "欢迎使用 ChmlFrp 社区工具箱 Daemon！"
    echo ""
    echo "接下来将引导你完成配置。你需要准备："
    echo "  1. 你的 proxyToken（从桌面客户端登录后获取）"
    echo "  2. 为这台服务器起一个名字（便于在设备列表中识别）"
    echo ""

    read -p "按 Enter 继续..." -r

    # 配置后端地址
    title "后端地址"
    local current_url
    current_url=$(config_get_backend_url)
    echo "当前后端地址: $current_url"
    read -p "是否修改？（直接 Enter 保持默认）: " -r new_url
    if [[ -n "$new_url" ]]; then
        config_set_backend_url "$new_url"
        success "后端地址已更新: $new_url"
    fi

    # 配置账号
    title "账号配置"

    local count
    count=$(config_count_accounts)
    if [[ "$count" -gt 0 ]] && config_is_configured; then
        echo "已检测到 $count 个已配置账号"
        echo ""
        read -p "是否添加新账号？[y/N] " -r
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
    echo "  后端地址: $(config_get_backend_url)"
    echo "  账号数量: $(config_count_accounts)"
    echo ""

    read -p "是否立即启动服务？[Y/n] " -r
    if [[ ! $REPLY =~ ^[Nn]$ ]]; then
        service_start
    fi
}

add_account_interactive() {
    echo ""
    echo "--- 添加账号 ---"
    echo ""
    echo "请输入 proxyToken（从桌面客户端获取）："
    echo "  获取方式：打开桌面客户端 → 设置 → 查看登录信息 → proxyToken"
    echo ""
    read -p "proxyToken: " -r token
    if [[ -z "$token" ]]; then
        warn "proxyToken 不能为空，已取消"
        return 1
    fi
    local default_name
    default_name=$(hostname 2>/dev/null || echo "服务器")
    read -p "设备名称 [${default_name}]: " -r name
    name="${name:-$default_name}"
    config_add_account "$token" "$name"
    success "账号已添加: $name"
}

# ===== 卸载 =====
uninstall() {
    title "卸载"
    info "开始卸载 $APP_NAME..."

    systemctl stop "$APP_NAME" 2>/dev/null || true
    systemctl disable "$APP_NAME" 2>/dev/null || true

    rm -f "$INSTALL_DIR/$APP_NAME"
    rm -f "$SERVICE_FILE"
    rm -rf "$CONFIG_DIR"

    # 仅在非 root 降级模式下清理用户/组
    if [[ "$APP_USER" != "root" ]] && id "$APP_USER" &>/dev/null; then
        userdel "$APP_USER" 2>/dev/null || true
    fi
    if [[ "$APP_GROUP" != "root" ]] && getent group "$APP_GROUP" &>/dev/null; then
        groupdel "$APP_GROUP" 2>/dev/null || true
    fi

    systemctl daemon-reload

    if [[ -d "$DATA_DIR" ]]; then
        read -p "是否删除数据目录 $DATA_DIR？[y/N] " -r
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
