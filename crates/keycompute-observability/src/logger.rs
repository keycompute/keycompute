use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// 日志初始化状态标志
static LOGGER_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// 检查日志系统是否已初始化
///
/// 注意：此函数只检查本模块是否已调用过初始化函数，
/// 不代表全局 tracing subscriber 的实际状态。
/// 如果其他代码设置了全局 subscriber，此函数仍可能返回 false。
pub fn is_logger_initialized() -> bool {
    LOGGER_INITIALIZED.load(Ordering::SeqCst)
}

/// 尝试初始化日志系统
///
/// 返回 `true` 表示初始化成功或日志系统已经可用。
/// 返回 `false` 表示初始化失败（极少见）。
///
/// 此函数是线程安全的，可以安全地多次调用。
/// 即使全局 subscriber 已被其他代码设置，此函数也不会 panic。
pub fn try_init_logger() -> bool {
    // 使用 compare_exchange 实现原子性的检查和设置，避免竞态条件
    // 如果已经是 true，直接返回成功
    if LOGGER_INITIALIZED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // 本模块已初始化过
        return true;
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info")
            .add_directive("keycompute=info".parse().unwrap())
            .add_directive("tower_http=info".parse().unwrap())
    });

    // 使用 try_init 避免在全局 subscriber 已存在时 panic
    match tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .try_init()
    {
        Ok(_) => {
            // 初始化成功
            true
        }
        Err(_) => {
            // 全局 subscriber 可能已被其他代码设置
            // tracing 全局 subscriber 一旦设置就无法更改
            // 这种情况下日志系统已经可用，视为成功
            true
        }
    }
}

/// 初始化日志系统
///
/// 使用 tracing-subscriber 配置结构化日志输出，支持 JSON 格式。
/// 环境变量 KEYCOMPUTE_LOG 控制日志级别，默认为 info。
///
/// 此函数是线程安全的，可以安全地多次调用。如果日志系统已经初始化
/// （无论是本模块还是其他代码），后续调用会静默跳过。
///
/// # Examples
///
/// ```
/// use keycompute_observability::init_logger;
/// init_logger();
/// ```
pub fn init_logger() {
    // 使用 compare_exchange 实现原子性的检查和设置，避免竞态条件
    if LOGGER_INITIALIZED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // 本模块已初始化过，静默跳过
        return;
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info")
            .add_directive("keycompute=info".parse().unwrap())
            .add_directive("tower_http=info".parse().unwrap())
    });

    // 使用 try_init 避免在全局 subscriber 已存在时 panic
    // 如果其他代码已经设置了全局 subscriber，这会静默失败，但日志系统已经可用
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().json())
        .try_init();
}

/// 初始化开发环境日志（人类可读格式）
///
/// 适用于本地开发，输出格式更易读。
/// 此函数是线程安全的，可以安全地多次调用。如果日志系统已经初始化
/// （无论是本模块还是其他代码），后续调用会静默跳过。
pub fn init_dev_logger() {
    // 使用 compare_exchange 实现原子性的检查和设置，避免竞态条件
    if LOGGER_INITIALIZED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // 本模块已初始化过，静默跳过
        return;
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("debug").add_directive("keycompute=debug".parse().unwrap())
    });

    // 使用 try_init 避免在全局 subscriber 已存在时 panic
    // 如果其他代码已经设置了全局 subscriber，这会静默失败，但日志系统已经可用
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().compact())
        .try_init();
}

/// 初始化测试环境日志
///
/// 仅在测试时启用，避免污染测试输出
#[cfg(test)]
pub fn init_test_logger() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .try_init();
}
