use std::any::TypeId;
use std::marker::PhantomData;
use std::sync::Arc;

/// 限定名(D33):双轨——静态路径零分配,动态路径(Arc)承接运行时名字。
/// PartialEq/Eq/Hash 按**字符串内容**:Static 与 Dynamic 同名等值互通;
/// Debug 同样只显示内容(不显示变体),与 Eq 语义一致——变体是实现细节,
/// 打出来会误导调试。
#[derive(Clone)]
pub(crate) enum Qualifier {
    Static(&'static str),
    Dynamic(Arc<str>),
}

impl Qualifier {
    fn as_str(&self) -> &str {
        match self {
            Qualifier::Static(s) => s,
            Qualifier::Dynamic(s) => s,
        }
    }
}

impl std::fmt::Debug for Qualifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl PartialEq for Qualifier {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Qualifier {}

impl std::hash::Hash for Qualifier {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

/// 服务键 = TypeId + 可选限定名(D21:`ServiceKey = TypeKey`)。
/// D33:限定名放宽为双轨,支持运行时构造的名字(桥事件等);
/// 代价是失去 `Copy`——克隆装载/注册路径,频率低。
/// 携带构造时捕获的类型名,仅用于诊断(`describe`/错误消息);
/// 相等与哈希仍只由 TypeId + 限定名决定(类型名是 TypeId 的纯函数)。
pub struct TypeKey {
    type_id: TypeId,
    type_name: &'static str,
    qualifier: Option<Qualifier>,
}

impl Clone for TypeKey {
    fn clone(&self) -> Self {
        Self {
            type_id: self.type_id,
            type_name: self.type_name,
            qualifier: self.qualifier.clone(),
        }
    }
}

impl PartialEq for TypeKey {
    fn eq(&self, other: &Self) -> bool {
        self.type_id == other.type_id && self.qualifier == other.qualifier
    }
}

impl Eq for TypeKey {}

impl std::hash::Hash for TypeKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_id.hash(state);
        self.qualifier.hash(state);
    }
}

impl std::fmt::Debug for TypeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.qualifier {
            Some(q) => write!(f, "{}#{q:?}", self.type_name),
            None => f.write_str(self.type_name),
        }
    }
}

impl TypeKey {
    /// 类型主键(默认,无限定名)。
    pub fn of<T: ?Sized + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            qualifier: None,
        }
    }

    /// 带限定名的键:同接口多实例(shaku Keyed 模式)。静态名零分配。
    pub fn keyed<T: ?Sized + 'static>(qualifier: &'static str) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            qualifier: Some(Qualifier::Static(qualifier)),
        }
    }

    /// 带动态限定名的键(D33):运行时构造的名字(桥事件名等)。
    /// 与 `keyed` 同名等值互通。
    pub fn keyed_dynamic<T: ?Sized + 'static>(name: impl Into<Arc<str>>) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            qualifier: Some(Qualifier::Dynamic(name.into())),
        }
    }

    /// 诊断描述(不参与分发):类型名 + 限定名(0.2.2 起携带构造时捕获的
    /// `type_name::<T>()`,错误消息与装配图可读)。
    pub fn describe(&self) -> String {
        match &self.qualifier {
            Some(q) => format!("{}#{}", self.type_name, q.as_str()),
            None => self.type_name.to_string(),
        }
    }
}

/// 服务键别名(D21 裁决:`ServiceKey` 不另建类型)。
pub type ServiceKey = TypeKey;

/// 类型化限定名常量(开放问题 2 的裁决:`Key<T>` newtype,shaku Keyed 精神)。
///
/// ```
/// use rutis::{Key, TypeKey};
/// const PRIMARY: Key<str> = Key::new("primary");
/// let k: TypeKey = PRIMARY.into();
/// assert_eq!(k, TypeKey::keyed::<str>("primary"));
/// ```
pub struct Key<T: ?Sized + 'static> {
    name: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized + 'static> Key<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized + 'static> From<Key<T>> for TypeKey {
    fn from(k: Key<T>) -> Self {
        TypeKey::keyed::<T>(k.name)
    }
}

/// isolate 作用域标识:同 label 字符串合并为同一作用域(TS 语义,D21)。
pub(crate) type ScopeId = std::sync::Arc<str>;

#[cfg(test)]
mod describe_tests {
    use super::*;

    struct ReadableService;

    /// 0.2.2:describe 携带构造时捕获的类型名与限定名,诊断可读。
    #[test]
    fn describe_uses_type_name_and_qualifier() {
        let plain = TypeKey::of::<ReadableService>();
        assert!(plain.describe().contains("ReadableService"));
        let keyed = TypeKey::keyed_dynamic::<ReadableService>("session-1/main");
        assert_eq!(
            keyed.describe(),
            format!("{}#session-1/main", std::any::type_name::<ReadableService>())
        );
        // 相等仍只由 TypeId + 限定名决定;诊断字段不参与。
        assert_eq!(plain, TypeKey::of::<ReadableService>());
        assert_ne!(plain, keyed);
    }
}
