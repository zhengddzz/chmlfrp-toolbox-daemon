//! delete_my_data 命令 - 删除当前用户在本设备上的所有数据

use tracing::info;
use super::CommandContext;

pub async fn handle(ctx: &CommandContext) -> super::CommandResult {
    let user_id = match ctx.user_id {
        Some(id) => id,
        None => {
            return Err(super::RpcError::new(
                "EXEC_FAILED",
                "无法确定 user_id，不能删除数据",
            ));
        }
    };

    info!("[delete_my_data] 删除用户 {} 的数据", user_id);

    crate::db::delete_user_data(&ctx.data_dir, user_id)
        .map_err(|e| super::RpcError::new("EXEC_FAILED", format!("删除数据失败: {}", e)))?;

    Ok(serde_json::json!({ "deleted": true }))
}
