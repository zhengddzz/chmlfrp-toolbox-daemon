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
