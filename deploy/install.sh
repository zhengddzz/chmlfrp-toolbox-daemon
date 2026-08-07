#!/bin/bash
#=============================================================
# ChmlFrp 社区工具箱 Daemon 一键安装脚本
#
# 功能：
#   1. 下载并安装 Daemon（deb 包）
#   2. 安装后自动引导用户配置 proxyToken
#   3. 部署管理菜单脚本到 /usr/local/bin/chmlfrp-toolbox
#
# 用法：
#   sudo bash install.sh                    # 安装（安装后自动引导配置）
#   sudo bash install.sh --local <path>     # 使用本地 deb 包安装
#   sudo bash install.sh --uninstall        # 卸载
#
# 在线一键安装：
#   curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh | sudo bash
#=============================================================

set -e

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
GITHUB_API="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
DEFAULT_BACKEND_URL="wss://api.cct.zdzz.top"

# 管理菜单脚本安装位置（安装到 PATH 中，用户可直接 chmlfrp-toolbox 运行）
MANAGE_SCRIPT_PATH="/usr/local/bin/chmlfrp-toolbox"
# 管理菜单脚本下载地址（从后端获取，带项目名前缀避免与其他项目冲突）
MANAGE_SCRIPT_URL="https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh"

# ===== 输出函数 =====
info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
error()   { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }
title()   { echo -e "\n${BOLD}${CYAN}=== $1 ===${NC}\n"; }

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

# ===== 用户/目录管理 =====
create_user() {
    if ! getent group "$APP_GROUP" &>/dev/null; then
        groupadd --system "$APP_GROUP"
        info "创建用户组: $APP_GROUP"
    fi
    if ! id "$APP_USER" &>/dev/null; then
        useradd --system --no-create-home --shell /usr/sbin/nologin --gid "$APP_GROUP" "$APP_USER"
        info "创建用户: $APP_USER"
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
        curl -fsSL -o "$output" "$url"
    elif command -v wget &>/dev/null; then
        wget -q -O "$output" "$url"
    else
        error "需要 curl 或 wget 来下载文件"
    fi
}

download_from_github() {
    local arch=$1
    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    info "正在获取最新版本信息..."
    local release_info
    release_info=$(download_file "$GITHUB_API" "/dev/stdout" 2>/dev/null || echo "")

    if [[ -z "$release_info" ]]; then
        error "无法获取 Release 信息，请检查网络连接"
    fi

    local deb_pattern
    case "$arch" in
        x64)   deb_pattern="_amd64.deb" ;;
        arm64) deb_pattern="_arm64.deb" ;;
    esac

    local download_url
    download_url=$(echo "$release_info" | grep -o "https://[^\"]*${deb_pattern}" | head -1)

    if [[ -z "$download_url" ]]; then
        error "未找到架构 ${arch} 的安装包"
    fi

    local deb_file="${tmp_dir}/${APP_NAME}.deb"
    info "正在下载: $download_url"
    download_file "$download_url" "$deb_file"
    echo "$deb_file"
}

# ===== 安装 =====
install_deb() {
    local deb_file=$1
    info "正在安装..."
    dpkg -i "$deb_file" || {
        warn "依赖缺失，尝试自动修复..."
        apt-get install -f -y || error "依赖安装失败，请手动运行: apt-get install -f"
    }
    success "安装完成"
}

install_rpm() {
    local rpm_file=$1
    info "正在安装..."
    if command -v dnf &>/dev/null; then
        dnf install -y "$rpm_file"
    elif command -v yum &>/dev/null; then
        yum install -y "$rpm_file"
    else
        error "无法找到 RPM 包管理器"
    fi
    success "安装完成"
}

# ===== 部署管理菜单脚本 =====
deploy_manage_script() {
    info "部署管理菜单脚本..."

    # 尝试从后端下载 manage.sh
    if download_file "$MANAGE_SCRIPT_URL" "$MANAGE_SCRIPT_PATH" 2>/dev/null; then
        chmod +x "$MANAGE_SCRIPT_PATH"
        success "管理菜单已安装: $MANAGE_SCRIPT_PATH"
    else
        # 后端不可用时，尝试从本地 deploy 目录复制（开发环境）
        local local_script
        local_script="$(dirname "$0")/manage.sh"
        if [[ -f "$local_script" ]]; then
            cp "$local_script" "$MANAGE_SCRIPT_PATH"
            chmod +x "$MANAGE_SCRIPT_PATH"
            success "管理菜单已安装（本地）: $MANAGE_SCRIPT_PATH"
        else
            warn "无法下载管理菜单脚本，请手动从 GitHub 获取"
        fi
    fi
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
    local count
    count=$(config_count_accounts)
    [[ "$count" -gt 0 ]]
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
[[accounts]]
proxy_token = ""
device_name = ""
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
    systemctl daemon-reload
    systemctl enable "$APP_NAME" 2>/dev/null || true
    if config_is_configured; then
        systemctl start "$APP_NAME"
        success "服务已启动"
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
    rm -f "$MANAGE_SCRIPT_PATH"
    rm -rf "$CONFIG_DIR"

    if id "$APP_USER" &>/dev/null; then
        userdel "$APP_USER" 2>/dev/null || true
    fi
    if getent group "$APP_GROUP" &>/dev/null; then
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
                echo "  --local <path>    使用本地 deb 包安装"
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
                deb_file=$(download_from_github "$arch")
                install_deb "$deb_file"
                ;;
            *)
                error "RPM 系统请使用 --local 参数指定本地 rpm 包"
                ;;
        esac
    fi

    generate_config
    create_data_dir
    deploy_manage_script

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
    info "管理菜单使用方法："
    echo "  sudo chmlfrp-toolbox          # 打开管理菜单"
    echo "  sudo chmlfrp-toolbox --help   # 查看帮助"
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
