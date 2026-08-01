use std::path::Path;
use std::rc::Rc;

use crate::keymap::{ActionRegistry, Keymaps, default_bindings};
use crate::theme::Config;

use super::LuaEngine;

/// Everything derived from the user's config directory, built as one unit.
pub struct LuaRuntime {
    pub config: Rc<Config>,
    pub engine: LuaEngine,
    pub keymaps: Keymaps,
}

impl LuaRuntime {
    /// Infallible: a broken `init.lua` yields a runtime carrying whatever
    /// registered before the error, reported via [`Self::init_error`].
    pub fn load(repo_path: &Path) -> Self {
        let config = Rc::new(crate::theme::load_config());
        let mut registry = ActionRegistry::new();
        let default_specs = default_bindings();
        let mut engine = LuaEngine::new(repo_path, &mut registry, &default_specs);
        let mut specs = default_specs;
        specs.extend(engine.take_extra_bindings());
        let keymaps = Keymaps::build(specs, registry);
        Self {
            config,
            engine,
            keymaps,
        }
    }

    /// The error that stopped `init.lua`, if it didn't run to completion.
    pub fn init_error(&self) -> Option<&str> {
        self.engine.init_error.as_deref()
    }
}
