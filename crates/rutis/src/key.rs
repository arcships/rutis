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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeKey {
    type_id: TypeId,
    qualifier: Option<Qualifier>,
}

impl TypeKey {
    /// 类型主键(默认,无限定名)。
    pub fn of<T: ?Sized + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            qualifier: None,
        }
    }

    /// 带限定名的键:同接口多实例(shaku Keyed 模式)。静态名零分配。
    pub fn keyed<T: ?Sized + 'static>(qualifier: &'static str) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            qualifier: Some(Qualifier::Static(qualifier)),
        }
    }

    /// 带动态限定名的键(D33):运行时构造的名字(桥事件名等)。
    /// 与 `keyed` 同名等值互通。
    pub fn keyed_dynamic<T: ?Sized + 'static>(name: impl Into<Arc<str>>) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            qualifier: Some(Qualifier::Dynamic(name.into())),
        }
    }

    /// 诊断描述(不参与分发)。
    pub fn describe(&self) -> String {
        match &self.qualifier {
            Some(q) => format!("{}#{}", self.type_id_debug(), q.as_str()),
            None => self.type_id_debug(),
        }
    }

    fn type_id_debug(&self) -> String {
        format!("{:?}", self.type_id)
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
