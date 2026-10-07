use thiserror::Error;

#[derive(Debug, Error)]
pub enum WError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("font error: {0}")]
    Font(String),
    #[error("unsupported / not yet implemented: {0}")]
    Unsupported(String),
    #[error("config error: {0}")]
    Config(String),
    /// 设备通信错误。
    ///
    /// 显示成「设备错误：…」而不是原来的英文 `device error: …`——
    /// 用户看到裸的 "device error" 完全不知道下一步该干什么，
    /// 具体原因与排查建议由调用方（如 `err_hint()`）补在里面。
    #[error("设备错误：{0}")]
    Device(String),
}

pub type Result<T> = std::result::Result<T, WError>;
