#!/bin/bash
#=============================================================
# ChmlFrp 社区工具箱 Daemon 管理菜单
#
# 类似 nezha.sh 的使用方式，安装后可直接运行：
#   sudo chmlfrp-toolbox          # 打开管理菜单
#   sudo chmlfrp-toolbox --help   # 查看帮助
#
# 也可通过安装脚本临时运行：
#   curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh | sudo bash
#
# 脚本版本: 1.0.0
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
DEFAULT_BACKEND_URL="wss://api.cct.zdzz.top"

# 脚本自身版本（每次发版需同步更新）
SCRIPT_VERSION="1.0.0"
# 脚本自身的下载地址（用于自更新）
SCRIPT_SELF_URL="https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh"
# 脚本版本信息接口（返回 JSON: version/changelog/updatedAt/size/sha256）
SCRIPT_INFO_URL="https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh/info"
# install.sh 下载地址（用于引导安装）
INSTALL_SCRIPT_URL="https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/install.sh"

# ===== 输出函数 =====
info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
success() { echo -e "${GREEN}[OK]${NC} $1"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
error()   { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }
title()   { echo -e "\n${BOLD}${CYAN}=== $1 ===${NC}\n"; }

# ===== 基础检查 =====
check_root() {
    if [[ $EUID -ne 0 ]]; then
        error "请使用 root 权限运行（sudo chmlfrp-toolbox）"
    fi
}

# 检查是否已安装
check_installed() {
    if ! command -v "$APP_NAME" &>/dev/null && [[ ! -f "$INSTALL_DIR/$APP_NAME" ]]; then
        warn "Daemon 尚未安装"
        echo ""
        read -p "是否立即安装？[Y/n] " -r
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            info "正在下载安装脚本..."
            if command -v curl &>/dev/null; then
                curl -fsSL "$INSTALL_SCRIPT_URL" | bash
            elif command -v wget &>/dev/null; then
                wget -qO- "$INSTALL_SCRIPT_URL" | bash
            else
                error "需要 curl 或 wget"
            fi
            exit 0
        else
            exit 0
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

config_delete_account() {
    local n=$1
    local total
    total=$(config_count_accounts)
    if [[ "$n" -lt 1 ]] || [[ "$n" -gt "$total" ]]; then
        return 1
    fi
    local line_num
    line_num=$(grep -n '^\s*\[\[accounts\]\]' "$CONFIG_FILE" | sed -n "${n}p" | cut -d: -f1)
    if [[ -z "$line_num" ]]; then
        return 1
    fi
    local end_line
    if [[ "$n" -lt "$total" ]]; then
        end_line=$(grep -n '^\s*\[\[accounts\]\]' "$CONFIG_FILE" | sed -n "$((n+1))p" | cut -d: -f1)
        end_line=$((end_line - 1))
    else
        end_line=$(wc -l < "$CONFIG_FILE")
    fi
    sed -i "${line_num},${end_line}d" "$CONFIG_FILE"
    sed -i '/^$/N;/^\n$/D' "$CONFIG_FILE"
}

config_set_account_token() {
    local n=$1
    local new_token=$2
    local line_num
    line_num=$(grep -n '^\s*\[\[accounts\]\]' "$CONFIG_FILE" | sed -n "${n}p" | cut -d: -f1)
    if [[ -z "$line_num" ]]; then
        return 1
    fi
    local token_line
    token_line=$(awk -v start="$line_num" 'NR>=start && /^\s*proxy_token\s*=/ {print NR; exit}' "$CONFIG_FILE")
    if [[ -n "$token_line" ]]; then
        sed -i "${token_line}s|.*|\    proxy_token = \"${new_token}\"|" "$CONFIG_FILE"
    fi
}

config_set_account_name() {
    local n=$1
    local new_name=$2
    local line_num
    line_num=$(grep -n '^\s*\[\[accounts\]\]' "$CONFIG_FILE" | sed -n "${n}p" | cut -d: -f1)
    if [[ -z "$line_num" ]]; then
        return 1
    fi
    local name_line
    name_line=$(awk -v start="$line_num" 'NR>=start && /^\s*device_name\s*=/ {print NR; exit}' "$CONFIG_FILE")
    if [[ -n "$name_line" ]]; then
        sed -i "${name_line}s|.*|\    device_name = \"${new_name}\"|" "$CONFIG_FILE"
    fi
}

# ===== 服务管理 =====
service_restart() {
    if ! systemctl is-enabled "$APP_NAME" &>/dev/null; then
        systemctl enable "$APP_NAME"
    fi
    systemctl restart "$APP_NAME"
    success "服务已重启"
}

service_start() {
    systemctl daemon-reload
    systemctl enable "$APP_NAME" 2>/dev/null || true
    systemctl start "$APP_NAME"
    success "服务已启动"
}

service_stop() {
    systemctl stop "$APP_NAME" 2>/dev/null || true
    info "服务已停止"
}

service_status() {
    echo ""
    systemctl status "$APP_NAME" --no-pager 2>/dev/null || warn "服务未运行"
    echo ""
}

service_logs() {
    echo ""
    info "显示最近 50 行日志（Ctrl+C 退出实时跟踪）..."
    echo ""
    journalctl -u "$APP_NAME" --no-pager -n 50
    echo ""
    read -p "是否实时跟踪日志？[y/N] " -r
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        journalctl -u "$APP_NAME" -f
    fi
}

# ===== 交互式操作 =====

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

list_accounts() {
    local count
    count=$(config_count_accounts)
    if [[ "$count" -eq 0 ]]; then
        warn "暂无账号"
        return
    fi
    echo ""
    printf "%-4s %-20s %-30s\n" "序号" "设备名称" "Token（前8位...）"
    printf "%-4s %-20s %-30s\n" "----" "--------------------" "------------------------------"
    for ((i=1; i<=count; i++)); do
        local name token_short
        name=$(config_get_account_name "$i")
        token=$(config_get_account_token "$i")
        token_short="${token:0:8}..."
        printf "%-4s %-20s %-30s\n" "$i" "$name" "$token_short"
    done
    echo ""
}

modify_account_interactive() {
    list_accounts
    local count
    count=$(config_count_accounts)
    if [[ "$count" -eq 0 ]]; then
        return
    fi
    read -p "输入要修改的账号序号（1-${count}）: " -r n
    if ! [[ "$n" =~ ^[0-9]+$ ]] || [[ "$n" -lt 1 ]] || [[ "$n" -gt "$count" ]]; then
        warn "无效的序号"
        return
    fi
    local current_name current_token
    current_name=$(config_get_account_name "$n")
    current_token=$(config_get_account_token "$n")
    echo ""
    echo "当前设备名称: $current_name"
    read -p "新设备名称（直接 Enter 保持不变）: " -r new_name
    if [[ -n "$new_name" ]]; then
        config_set_account_name "$n" "$new_name"
        success "设备名称已更新: $new_name"
    fi
    echo ""
    echo "当前 Token: ${current_token:0:8}..."
    read -p "新 proxyToken（直接 Enter 保持不变）: " -r new_token
    if [[ -n "$new_token" ]]; then
        config_set_account_token "$n" "$new_token"
        success "proxyToken 已更新"
    fi
}

delete_account_interactive() {
    list_accounts
    local count
    count=$(config_count_accounts)
    if [[ "$count" -eq 0 ]]; then
        return
    fi
    read -p "输入要删除的账号序号（1-${count}）: " -r n
    if ! [[ "$n" =~ ^[0-9]+$ ]] || [[ "$n" -lt 1 ]] || [[ "$n" -gt "$count" ]]; then
        warn "无效的序号"
        return
    fi
    local name
    name=$(config_get_account_name "$n")
    read -p "确认删除账号「$name」？[y/N] " -r
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        config_delete_account "$n"
        success "账号「$name」已删除"
    else
        info "已取消"
    fi
}

modify_backend_url() {
    local current_url
    current_url=$(config_get_backend_url)
    echo ""
    echo "当前后端地址: $current_url"
    read -p "新后端地址（直接 Enter 保持不变）: " -r new_url
    if [[ -n "$new_url" ]]; then
        config_set_backend_url "$new_url"
        success "后端地址已更新: $new_url"
    fi
}

view_config() {
    title "当前配置"
    echo "后端地址: $(config_get_backend_url)"
    echo "数据目录: $DATA_DIR"
    echo ""
    list_accounts
}

test_connection() {
    title "测试连接"
    local backend_url
    backend_url=$(config_get_backend_url)
    local http_url
    http_url="${backend_url/wss:\/\//https:\/\/}"
    http_url="${http_url/ws:\/\//http:\/\/}"
    info "测试后端连通性: $http_url"
    if curl -fsSL --connect-timeout 5 -o /dev/null -w "%{http_code}" "$http_url" 2>/dev/null; then
        success "后端连接正常"
    else
        warn "后端连接失败，请检查网络或后端地址"
    fi
    local count
    count=$(config_count_accounts)
    if [[ "$count" -eq 0 ]]; then
        warn "未配置账号，跳过 Token 测试"
        return
    fi
    echo ""
    info "测试 Token 有效性..."
    for ((i=1; i<=count; i++)); do
        local token name
        token=$(config_get_account_token "$i")
        name=$(config_get_account_name "$i")
        if [[ -z "$token" ]]; then
            warn "[$i] $name - Token 为空"
            continue
        fi
        local resp
        resp=$(curl -fsSL --connect-timeout 5 -H "Authorization: Bearer $token" "${http_url}/api/devices" 2>&1) || true
        if echo "$resp" | grep -q '"devices"'; then
            success "[$i] $name - Token 有效"
        elif echo "$resp" | grep -q 'INVALID_TOKEN\|令牌无效'; then
            warn "[$i] $name - Token 无效或已过期"
        else
            warn "[$i] $name - 无法验证（$resp）"
        fi
    done
}

# ===== 版本比较 =====
# 返回 0 表示 $1 > $2（$1 是新版本）
version_gt() {
    if [[ "$1" == "$2" ]]; then
        return 1
    fi
    local higher
    higher=$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)
    [[ "$higher" == "$1" ]]
}

# ===== 通用下载工具 =====
download_to() {
    local url=$1
    local output=$2
    if command -v curl &>/dev/null; then
        curl -fsSL --connect-timeout 10 -o "$output" "$url"
    elif command -v wget &>/dev/null; then
        wget -q --timeout=30 -O "$output" "$url"
    else
        return 1
    fi
}

# ===== 检查管理菜单脚本更新 =====
check_script_update() {
    title "检查管理菜单脚本更新"
    info "当前版本: v$SCRIPT_VERSION"
    info "正在查询最新版本..."

    local resp
    resp=$(curl -fsSL --connect-timeout 10 "$SCRIPT_INFO_URL" 2>/dev/null) || {
        warn "无法获取版本信息（网络错误或后端不可达）"
        return 1
    }

    # 解析 JSON 字段（兼容无 jq 的环境）
    local latest_version changelog updated_at
    latest_version=$(echo "$resp" | grep -oE '"version"\s*:\s*"[^"]*"' | head -1 | sed -E 's/.*"version"\s*:\s*"([^"]*)".*/\1/')
    changelog=$(echo "$resp" | grep -oE '"changelog"\s*:\s*"[^"]*"' | head -1 | sed -E 's/.*"changelog"\s*:\s*"([^"]*)".*/\1/')
    updated_at=$(echo "$resp" | grep -oE '"updatedAt"\s*:\s*"[^"]*"' | head -1 | sed -E 's/.*"updatedAt"\s*:\s*"([^"]*)".*/\1/')

    if [[ -z "$latest_version" ]]; then
        warn "无法解析版本信息"
        return 1
    fi

    info "最新版本: v$latest_version（更新于 ${updated_at:-未知}）"
    if [[ -n "$changelog" ]]; then
        echo -e "  更新说明: ${changelog}"
    fi

    if version_gt "$latest_version" "$SCRIPT_VERSION"; then
        echo ""
        warn "发现新版本：v$SCRIPT_VERSION → v$latest_version"
        read -p "是否立即更新管理菜单脚本？[Y/n] " -r
        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            self_update_script "$latest_version"
        else
            info "已跳过更新"
        fi
    else
        success "已是最新版本"
    fi
}

# ===== 自更新管理菜单脚本 =====
self_update_script() {
    local new_version=$1
    info "正在下载最新版本 v${new_version}..."

    local tmp_file
    tmp_file=$(mktemp)
    trap "rm -f '$tmp_file'" EXIT

    if ! download_to "$SCRIPT_SELF_URL" "$tmp_file"; then
        warn "下载失败，请稍后重试"
        return 1
    fi

    # 基本校验：确保是 bash 脚本
    if ! head -1 "$tmp_file" | grep -q '^#!.*bash'; then
        warn "下载的文件不是有效的 bash 脚本，已取消更新"
        return 1
    fi

    # 覆盖自身（$0 是当前脚本路径，通常是 /usr/local/bin/chmlfrp-toolbox）
    local self_path
    self_path=$(readlink -f "$0" 2>/dev/null || echo "$0")
    if [[ ! -w "$self_path" ]]; then
        warn "无写入权限: $self_path"
        return 1
    fi

    cp "$tmp_file" "$self_path"
    chmod +x "$self_path"
    success "管理菜单已更新到 v$new_version"
    echo ""
    info "请重新运行: sudo chmlfrp-toolbox"
    exit 0
}

# ===== 检查 Daemon 二进制更新 =====
check_daemon_update() {
    title "检查 Daemon 更新"
    local current_version
    current_version=$("$INSTALL_DIR/$APP_NAME" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1) || current_version="未知"
    info "当前 Daemon 版本: v${current_version}"

    info "正在查询最新版本..."
    # 优先走后端代理接口（避免服务器无法访问 GitHub）
    local release_url="https://api.github.com/repos/zhengddzz/${APP_NAME}/releases/latest"
    local release_info
    release_info=$(curl -fsSL --connect-timeout 10 "$release_url" 2>/dev/null) || {
        warn "无法获取版本信息（网络错误或无法访问 GitHub）"
        return 1
    }

    local latest_version release_name
    latest_version=$(echo "$release_info" | grep -oE '"tag_name"\s*:\s*"[^"]*"' | head -1 | sed -E 's/.*"tag_name"\s*:\s*"([^"]*)".*/\1/' | sed 's/^v//')
    release_name=$(echo "$release_info" | grep -oE '"name"\s*:\s*"[^"]*"' | head -1 | sed -E 's/.*"name"\s*:\s*"([^"]*)".*/\1/')

    if [[ -z "$latest_version" ]]; then
        warn "无法解析版本信息"
        return 1
    fi

    info "最新版本: v$latest_version（${release_name:-ChmlFrp 社区工具箱}）"

    if [[ "$current_version" != "未知" ]] && version_gt "$latest_version" "$current_version"; then
        echo ""
        warn "发现新版本：v${current_version} → v$latest_version"
        echo ""
        info "Daemon 更新方式："
        echo "  1. 一键更新（推荐）："
        echo "     curl -fsSL $INSTALL_SCRIPT_URL | sudo bash"
        echo "  2. 手动下载 deb 包："
        echo "     https://github.com/zhengddzz/${APP_NAME}/releases/latest"
        echo ""
        read -p "是否立即执行一键更新？[y/N] " -r
        if [[ $REPLY =~ ^[Yy]$ ]]; then
            info "正在执行一键更新（将重启服务）..."
            if command -v curl &>/dev/null; then
                curl -fsSL "$INSTALL_SCRIPT_URL" | bash
            else
                wget -qO- "$INSTALL_SCRIPT_URL" | bash
            fi
        fi
    else
        success "已是最新版本"
    fi
}

# ===== 检查更新子菜单 =====
check_update_menu() {
    while true; do
        title "检查更新"
        echo "  1) 检查管理菜单脚本更新（当前 v$SCRIPT_VERSION）"
        echo "  2) 检查 Daemon 二进制更新"
        echo "  0) 返回主菜单"
        echo ""
        read -p "请选择 [0-2]: " -r sub
        case "$sub" in
            1) check_script_update ;;
            2) check_daemon_update ;;
            0) return ;;
            *) warn "无效选择" ;;
        esac
        echo ""
        read -p "按 Enter 继续..." -r
    done
}

# ===== 卸载 =====
uninstall() {
    title "卸载"
    info "开始卸载 $APP_NAME..."
    systemctl stop "$APP_NAME" 2>/dev/null || true
    systemctl disable "$APP_NAME" 2>/dev/null || true
    rm -f "$INSTALL_DIR/$APP_NAME"
    rm -f "$SERVICE_FILE"
    rm -f /usr/local/bin/chmlfrp-toolbox
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
    exit 0
}

# ===== 主菜单 =====
show_menu() {
    while true; do
        title "ChmlFrp 工具箱 Daemon 管理菜单"

        local service_state
        if systemctl is-active "$APP_NAME" &>/dev/null; then
            service_state="${GREEN}运行中${NC}"
        else
            service_state="${RED}已停止${NC}"
        fi
        local account_count
        account_count=$(config_count_accounts)
        echo -e "  服务状态: ${service_state}    账号数量: ${account_count}"
        echo ""

        echo "  1) 查看配置"
        echo "  2) 添加账号"
        echo "  3) 修改账号"
        echo "  4) 删除账号"
        echo "  5) 修改后端地址"
        echo "  6) 测试连接"
        echo "  7) 启动服务"
        echo "  8) 停止服务"
        echo "  9) 重启服务"
        echo " 10) 查看运行状态"
        echo " 11) 查看日志"
        echo " 12) 卸载"
        echo " 13) 检查更新"
        echo "  0) 退出"
        echo ""

        read -p "请选择 [0-13]: " -r choice

        case "$choice" in
            1) view_config ;;
            2) add_account_interactive ;;
            3) modify_account_interactive ;;
            4) delete_account_interactive ;;
            5) modify_backend_url ;;
            6) test_connection ;;
            7) service_start ;;
            8) service_stop ;;
            9) service_restart ;;
            10) service_status ;;
            11) service_logs ;;
            12)
                read -p "确认卸载？[y/N] " -r
                if [[ $REPLY =~ ^[Yy]$ ]]; then
                    uninstall
                fi
                ;;
            13) check_update_menu ;;
            0)
                info "退出管理菜单"
                exit 0
                ;;
            *)
                warn "无效选择"
                ;;
        esac

        echo ""
        read -p "按 Enter 返回菜单..." -r
    done
}

# ===== 入口 =====
main() {
    check_root
    check_installed

    # 直接进入菜单
    show_menu
}

# 支持 --help
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    echo "ChmlFrp 社区工具箱 Daemon 管理菜单 v$SCRIPT_VERSION"
    echo ""
    echo "用法:"
    echo "  sudo chmlfrp-toolbox          # 打开管理菜单"
    echo "  sudo chmlfrp-toolbox --help   # 显示此帮助"
    echo ""
    echo "也可通过 curl 临时运行（无需安装）："
    echo "  curl -fsSL https://api.cct.zdzz.top/chmlfrp-toolbox-daemon/manage.sh | sudo bash"
    exit 0
fi

main "$@"
