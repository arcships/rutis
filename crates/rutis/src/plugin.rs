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
/// 与 [`Plugin`] 的差异:config 级校验由 `validate_config` 承担(实例级
/// `Plugin::validate` 仍在装载期执行)。
///
/// 依赖声明与 [`Plugin::injects`] 同形:**静态**,spawn 时注册一次、终身
/// 不变(D32f 修订,对齐 cordis:TS 的 `inject` 是 fiber 构造固化字段,
/// `update(config)` 从不改变声明)。此前"从 config 派生"的设计没有用例
/// 支撑,且引入了漂移/基线/恢复一整族无法收敛的边界(三轮评审记录,
/// §八-§十);按配置选依赖的标准形态是拆成多个插件、配置决定装哪个。
pub trait PluginFactory<C: Send + Sync + 'static>: Send + Sync + 'static {
    /// 显示名(日志/诊断,fiber 创建时取用,不再随代变化)。
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// 依赖门控声明(静态,spawn 时注册一次;与 [`Plugin::injects`] 对称)。
    fn injects(&self) -> &[TypeKey] {
        &[]
    }

    /// config 级校验(不构造实例;`update` 的 dry-run 第一步)。
    fn validate_config(&self, _config: &C) -> Result<(), CordisError> {
        Ok(())
    }

    /// 构造插件实例。失败 = config 无法产出可用实例(装载期走 `fail_load`)。
    fn build(&self, config: &C) -> Result<Box<dyn Plugin>, CordisError>;
}
