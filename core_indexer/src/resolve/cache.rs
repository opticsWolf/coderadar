// CodeRadar v3.6 — Resolution Cache (§5.4)
// v3.6: EntityId-based keys instead of SlotMap u64 raw keys.

use std::collections::HashMap;

use crate::types::*;

pub struct ResolutionCache {
    /// (entity_id, name) → Resolution
    pub name_in_module: HashMap<(EntityId, String), Resolution>,
    /// (class_id, method_name) → FunctionId
    pub method_in_class: HashMap<(EntityId, String), EntityId>,
    /// (module_id, import_target) → ImportResolution
    pub import_target: HashMap<(EntityId, String), ImportResolution>,
}

#[derive(Clone, Debug)]
pub enum Resolution {
    Symbol(SymbolId),
    External,
    Unresolved(UnresolvedReason),
}

impl ResolutionCache {
    pub fn new() -> Self {
        Self {
            name_in_module: HashMap::new(),
            method_in_class: HashMap::new(),
            import_target: HashMap::new(),
        }
    }

    pub fn invalidate_module(&mut self, module_id: &str) {
        self.name_in_module.retain(|(m, _), _| m != module_id);
        self.import_target.retain(|(m, _), _| m != module_id);
    }

    pub fn invalidate_class(&mut self, class_id: &str) {
        self.method_in_class.retain(|(c, _), _| c != class_id);
    }

    /// Bounded invalidation: walk subclasses transitively up to max_depth.
    /// If max_depth is exceeded, flush the entire method_in_class cache to
    /// prevent O(n²) blowup on pathological hierarchies.
    pub fn invalidate_class_hierarchy(
        &mut self,
        class_id: &str,
        all_subclasses: &[EntityId],
        max_depth: usize,
    ) {
        if all_subclasses.len() > max_depth {
            // Pathological hierarchy — flush all to avoid O(n²)
            self.method_in_class.clear();
        } else {
            self.invalidate_class(class_id);
            for sub_id in all_subclasses {
                self.invalidate_class(sub_id);
            }
        }
    }

    pub fn clear(&mut self) {
        self.name_in_module.clear();
        self.method_in_class.clear();
        self.import_target.clear();
    }

    pub fn get_name_in_module(&self, module_id: &str, name: &str) -> Option<&Resolution> {
        self.name_in_module.get(&(module_id.to_string(), name.to_string()))
    }

    pub fn set_name_in_module(&mut self, module_id: &str, name: &str, resolution: Resolution) {
        self.name_in_module.insert((module_id.to_string(), name.to_string()), resolution);
    }

    pub fn get_method_in_class(&self, class_id: &str, method: &str) -> Option<&EntityId> {
        self.method_in_class.get(&(class_id.to_string(), method.to_string()))
    }

    pub fn set_method_in_class(&mut self, class_id: &str, method: &str, function_id: &str) {
        self.method_in_class
            .insert((class_id.to_string(), method.to_string()), function_id.to_string());
    }

    pub fn get_import_target(&self, module_id: &str, target: &str) -> Option<&ImportResolution> {
        self.import_target.get(&(module_id.to_string(), target.to_string()))
    }

    pub fn set_import_target(&mut self, module_id: &str, target: &str, resolution: ImportResolution) {
        self.import_target.insert((module_id.to_string(), target.to_string()), resolution);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UnresolvedReason;

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module("mod1", "my_func", Resolution::Symbol(SymbolId::Function("f1".into())));
        let result = cache.get_name_in_module("mod1", "my_func");
        assert!(result.is_some());
    }

    #[test]
    fn test_cache_miss() {
        let cache = ResolutionCache::new();
        assert!(cache.get_name_in_module("mod1", "nope").is_none());
    }

    #[test]
    fn test_invalidate_module() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module("mod1", "a", Resolution::External);
        cache.set_name_in_module("mod2", "b", Resolution::External);
        cache.invalidate_module("mod1");
        assert!(cache.get_name_in_module("mod1", "a").is_none());
        assert!(cache.get_name_in_module("mod2", "b").is_some());
    }

    #[test]
    fn test_invalidate_class() {
        let mut cache = ResolutionCache::new();
        cache.set_method_in_class("cls1", "foo", "f1");
        cache.set_method_in_class("cls2", "bar", "f2");
        cache.invalidate_class("cls1");
        assert!(cache.get_method_in_class("cls1", "foo").is_none());
        assert!(cache.get_method_in_class("cls2", "bar").is_some());
    }

    #[test]
    fn test_invalidate_class_hierarchy_bounded() {
        let mut cache = ResolutionCache::new();
        cache.set_method_in_class("c1", "m1", "f1");
        cache.set_method_in_class("c2", "m2", "f2");

        let subclasses: Vec<EntityId> = vec!["c2".into(), "c3".into()];
        cache.invalidate_class_hierarchy("c1", &subclasses, 10);
        assert!(cache.get_method_in_class("c1", "m1").is_none());
        assert!(cache.get_method_in_class("c2", "m2").is_none());
    }

    #[test]
    fn test_invalidate_class_hierarchy_flush_on_overflow() {
        let mut cache = ResolutionCache::new();
        cache.set_method_in_class("c1", "m1", "f1");
        let subclasses: Vec<EntityId> = (0..100).map(|i| format!("c{}", i)).collect();
        cache.invalidate_class_hierarchy("root", &subclasses, 50);
        // Flushed due to max_depth exceeded
        assert!(cache.get_method_in_class("c1", "m1").is_none());
    }

    #[test]
    fn test_clear() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module("m", "x", Resolution::External);
        cache.clear();
        assert!(cache.get_name_in_module("m", "x").is_none());
    }
}
