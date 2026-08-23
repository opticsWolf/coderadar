// CodeRadar v3.6 — Extraction: Known-Decorator Table (§4.3)
// Maps decorator names to semantic effects (FunctionKind / EffectiveClass).

use crate::types::{EffectiveClass, FunctionKind};

/// Effect a decorator has on a class or function.
#[derive(Clone, Debug, PartialEq)]
pub enum DecoratorEffect {
    /// Sets the enclosing function's FunctionKind.
    FunctionKind(FunctionKind),
    /// Sets the enclosing class's EffectiveClass.
    ClassEffect(EffectiveClass),
    /// Synthesizes a field on the enclosing class.
    SynthesizedField { name: String, kind: String },
    /// Mark as property setter for a given property name.
    PropertySetterOf(String),
    /// Mark as property deleter for a given property name.
    PropertyDeleterOf(String),
}

/// Known-decorator table for Python (§4.3).
pub fn known_decorator_effects(decorator: &str) -> Option<DecoratorEffect> {
    match decorator {
        "@staticmethod" => Some(DecoratorEffect::FunctionKind(FunctionKind::StaticMethod)),
        "@classmethod" => Some(DecoratorEffect::FunctionKind(FunctionKind::ClassMethod)),
        "@property" => Some(DecoratorEffect::FunctionKind(FunctionKind::Property)),
        "@abstractmethod" => {
            Some(DecoratorEffect::FunctionKind(FunctionKind::AbstractMethod))
        }
        "@functools.cached_property" => {
            Some(DecoratorEffect::FunctionKind(FunctionKind::CachedProperty))
        }
        "@dataclass" | "@dataclass()" => Some(DecoratorEffect::ClassEffect(EffectiveClass::Dataclass {
            frozen: false,
            eq: true,
            order: false,
        })),
        "@dataclass(frozen=True)" | "@dataclass(frozen=True,)" => {
            Some(DecoratorEffect::ClassEffect(EffectiveClass::Dataclass {
                frozen: true,
                eq: true,
                order: false,
            }))
        }
        // Setter/deleter extracted by pattern matching — see below
        other if other.ends_with(".setter") => {
            let prop_name = other
                .trim_start_matches('@')
                .trim_end_matches(".setter")
                .to_string();
            Some(DecoratorEffect::PropertySetterOf(prop_name))
        }
        other if other.ends_with(".deleter") => {
            let prop_name = other
                .trim_start_matches('@')
                .trim_end_matches(".deleter")
                .to_string();
            Some(DecoratorEffect::PropertyDeleterOf(prop_name))
        }
        _ => None,
    }
}

/// Check if a decorator makes the class abstract.
pub fn is_abstract_decorator(decorator: &str) -> bool {
    decorator == "@abstractmethod"
}

/// Check if a decorator is a dataclass variant.
pub fn is_dataclass_decorator(decorator: &str) -> bool {
    decorator.starts_with("@dataclass")
}

/// Synthesized methods that dataclass decorators generate.
pub fn dataclass_synthesized_methods() -> Vec<(&'static str, Vec<(&'static str, Option<&'static str>)>)> {
    vec![
        ("__init__", vec![("self", None), ("*", None)]),
        ("__repr__", vec![("self", None)]),
        ("__eq__", vec![("self", None), ("other", None)]),
    ]
}

/// Check if a decorator string matches a known pattern that requires
/// special handling (e.g., class-level effects).
pub fn classify_decorator_impact(
    decorators: &[String],
) -> Option<EffectiveClass> {
    for d in decorators {
        match d.as_str() {
            s if s.starts_with("@dataclass") => {
                let frozen = s.contains("frozen=True") || s.contains("frozen=true");
                return Some(EffectiveClass::Dataclass {
                    frozen,
                    eq: true,
                    order: false,
                });
            }
            "@abstractmethod" => return Some(EffectiveClass::Abstract),
            _ => {}
        }
    }
    None
}
