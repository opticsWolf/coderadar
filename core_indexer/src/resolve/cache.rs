// CodeRadar v3.3 — Resolution Cache (§5.4)
// Caches resolution results with precise invalidation rules.

use std::collections::HashMap;

use crate::types::*;

/// Cached resolution results with precise invalidation.
pub struct ResolutionCache {
    /// (module_id, name) → Resolution
    pub name_in_module: HashMap<(u64, String), Resolution>,
    /// (class_id, method_name) → FunctionId
    pub method_in_class: HashMap<(u64, String), u64>,
    /// (module_id, import_target) → ImportResolution
    pub import_target: HashMap<(u64, String), ImportResolution>,
}

#[derive(Clone, Debug)]
pub enum Resolution {
    Symbol(u64), // SymbolId packed as u64
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

    /// Invalidate all cache entries for a specific module.
    pub fn invalidate_module(&mut self, module_key: u64) {
        self.name_in_module
            .retain(|(m, _), _| *m != module_key);
        self.import_target
            .retain(|(m, _), _| *m != module_key);
    }

    /// Invalidate all method-in-class entries for a given class.
    pub fn invalidate_class(&mut self, class_key: u64) {
        self.method_in_class
            .retain(|(c, _), _| *c != class_key);
    }

    /// Invalidate method-in-class entries for a class and its transitive
    /// subclasses (bounded at max_mro_invalidation_depth = 50).
    pub fn invalidate_class_hierarchy(
        &mut self,
        class_key: u64,
        all_subclasses: &[u64],
        max_depth: usize,
    ) {
        self.invalidate_class(class_key);
        for sub_key in all_subclasses.iter().take(max_depth) {
            self.invalidate_class(*sub_key);
        }
    }

    /// Clear the entire cache.
    pub fn clear(&mut self) {
        self.name_in_module.clear();
        self.method_in_class.clear();
        self.import_target.clear();
    }

    /// Get a cached name-in-module resolution.
    pub fn get_name_in_module(&self, module_key: u64, name: &str) -> Option<&Resolution> {
        self.name_in_module.get(&(module_key, name.to_string()))
    }

    pub fn set_name_in_module(&mut self, module_key: u64, name: &str, resolution: Resolution) {
        self.name_in_module
            .insert((module_key, name.to_string()), resolution);
    }

    /// Get a cached method-in-class resolution.
    pub fn get_method_in_class(&self, class_key: u64, method: &str) -> Option<&u64> {
        self.method_in_class
            .get(&(class_key, method.to_string()))
    }

    pub fn set_method_in_class(&mut self, class_key: u64, method: &str, function_id: u64) {
        self.method_in_class
            .insert((class_key, method.to_string()), function_id);
    }

    pub fn get_import_target(
        &self,
        module_key: u64,
        target: &str,
    ) -> Option<&ImportResolution> {
        self.import_target
            .get(&(module_key, target.to_string()))
    }

    pub fn set_import_target(
        &mut self,
        module_key: u64,
        target: &str,
        resolution: ImportResolution,
    ) {
        self.import_target
            .insert((module_key, target.to_string()), resolution);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UnresolvedReason;

    #[test]
    fn test_cache_insert_and_get_name_in_module() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module(1, "my_func", Resolution::Symbol(42));
        let result = cache.get_name_in_module(1, "my_func");
        assert!(result.is_some());
        match result.unwrap() {
            Resolution::Symbol(id) => assert_eq!(*id, 42),
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let cache = ResolutionCache::new();
        assert!(cache.get_name_in_module(1, "nonexistent").is_none());
    }

    #[test]
    fn test_invalidate_module_removes_entries() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module(1, "a", Resolution::Symbol(10));
        cache.set_name_in_module(2, "b", Resolution::Symbol(20));
        cache.set_import_target(1, "c", ImportResolution::Unresolved);

        cache.invalidate_module(1);
        assert!(cache.get_name_in_module(1, "a").is_none());
        assert!(cache.get_import_target(1, "c").is_none());
        // Module 2 untouched
        assert!(cache.get_name_in_module(2, "b").is_some());
    }

    #[test]
    fn test_invalidate_class_removes_method_entries() {
        let mut cache = ResolutionCache::new();
        cache.set_method_in_class(10, "foo", 100);
        cache.set_method_in_class(20, "bar", 200);

        cache.invalidate_class(10);
        assert!(cache.get_method_in_class(10, "foo").is_none());
        assert!(cache.get_method_in_class(20, "bar").is_some());
    }

    #[test]
    fn test_invalidate_class_hierarchy_bounded() {
        let mut cache = ResolutionCache::new();
        cache.set_method_in_class(1, "m1", 101);
        cache.set_method_in_class(2, "m2", 102);
        cache.set_method_in_class(3, "m3", 103);

        let subclasses = vec![2u64, 3u64, 4u64, 5u64, 6u64, 7u64];
        cache.invalidate_class_hierarchy(1, &subclasses, 2);
        assert!(cache.get_method_in_class(1, "m1").is_none());
        assert!(cache.get_method_in_class(2, "m2").is_none());
        assert!(cache.get_method_in_class(3, "m3").is_none());
    }

    #[test]
    fn test_clear_removes_all() {
        let mut cache = ResolutionCache::new();
        cache.set_name_in_module(1, "x", Resolution::External);
        cache.set_method_in_class(1, "y", 99);
        cache.set_import_target(1, "z", ImportResolution::Dynamic);

        cache.clear();
        assert!(cache.get_name_in_module(1, "x").is_none());
        assert!(cache.get_method_in_class(1, "y").is_none());
        assert!(cache.get_import_target(1, "z").is_none());
    }

    #[test]
    fn test_resolution_enum_variants() {
        let _r1 = Resolution::Symbol(42);
        let _r2 = Resolution::External;
        let _r3 = Resolution::Unresolved(UnresolvedReason::NameNotInScope);
    }
}
