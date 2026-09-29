pub mod app;
pub mod conflict;
pub mod dag;
pub mod diff_tool;
pub mod graph;
pub mod history;
pub mod idx;
pub mod input;
pub mod jj_command;
pub mod keymap;
pub mod lua;
pub mod repo;
pub mod repo_service;
pub mod selection;
pub mod terminal;
pub mod theme;
pub mod time;
pub mod types;
pub mod ui;

#[macro_export]
macro_rules! pluralize {
    ($value:expr, $singular:expr, $plural:expr) => {
        if $value == 1 { $singular } else { $plural }
    };
}
