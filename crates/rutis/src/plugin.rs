use crate::ctx::Ctx;
use crate::error::CordisError;
use crate::key::TypeKey;
use crate::{BoxFuture, Effect};

/// 插件 = 装配单元(支柱 1)。config 烘进实例(D19):
/// 具体插件在 `new(config)` 时持有配置,`validate` 校验自持那份。
pub trait Plugin: Send + Sync + 'static {
    /// 显示名(日志/诊断)。
    fn name(&self) -> &str;

    /// 依赖门控声明(支柱 2):全部就绪(存在 + provider Active + `check()` 通过)才启动。
    fn injects(&self) -> &[TypeKey] {
        &[]
    }

    /// 校验自持有 config(D12:validate-before-store,注册/装载期调用)。
    fn validate(&self) -> Result<(), CordisError> {
        Ok(())
    }

    /// 装配体:提供 0..n 服务、注册 0..n 监听、交回清理。
    fn apply<'a>(&'a self, ctx: &'a Ctx) -> BoxFuture<'a, Result<Effect, CordisError>>;
}

/// 插件工厂(D32:配置热更新):每代从当前 config 构造插件实例。
///
/// `build` 必须是纯构造(无副作用或幂等)——`FiberView::update` 的 dry-run
/// 与实际装载各调用一次,两次产物不要求同一实例但要求等价。
///
/// 与 [`Plugin`] 的差异:工厂模式无实例可问依赖,门控声明由
/// `injects(&config)` 从配置派生;config 级校验由 `validate_config`
/// 承担(实例级 `Plugin::validate` 仍在装载期执行)。
pub trait PluginFactory<C: Send + Sync + 'static>: Send + Sync + 'static {
    /// 显示名(日志/诊断,fiber 创建时取用,不再随代变化)。
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// 依赖门控声明(工厂模式:从 config 派生,spawn 时注册一次)。
    ///
    /// **必须对 config 稳定**:`FiberView::update` 会校验新 config 派生的
    /// 声明与 spawn 时集合相等,不等直接 `Validation` 拒绝——注册表只
    /// 在 spawn 注册一次,漂移会让 notify/驱逐静默失效。需要按配置改变
    /// 依赖的插件应拆成多个插件或声明超集。
    fn injects(&self, _config: &C) -> Vec<TypeKey> {
        Vec::new()
    }

    /// config 级校验(不构造实例;`update` 的 dry-run 第一步)。
    fn validate_config(&self, _config: &C) -> Result<(), CordisError> {
        Ok(())
    }

    /// 构造插件实例。失败 = config 无法产出可用实例(装载期走 `fail_load`)。
    fn build(&self, config: &C) -> Result<Box<dyn Plugin>, CordisError>;
}
