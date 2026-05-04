use tui_input::Input;

use crate::idx::RowIdx;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SearchScopes: u8 {
        const CHANGE_ID   = 1 << 0;
        const COMMIT_ID   = 1 << 1;
        const DESCRIPTION = 1 << 2;
        const BOOKMARK    = 1 << 3;
        const AUTHOR      = 1 << 4;
        const PATH_COMMAND = 1 << 5;
        const LINE        = 1 << 6;
        const TAG         = 1 << 7;
    }
}

impl SearchScopes {
    pub const DEFAULT: Self = Self::CHANGE_ID.union(Self::DESCRIPTION);
    pub const DEFAULT_BOOKMARK: Self = Self::BOOKMARK.union(Self::DESCRIPTION);
    pub const DEFAULT_TAG: Self = Self::TAG.union(Self::DESCRIPTION);
    pub const DEFAULT_OP_LOG: Self = Self::DESCRIPTION;
}

pub struct SearchScopeSpec {
    pub flag: SearchScopes,
    pub hint: &'static str,
    pub label: &'static str,
}

pub const SEARCH_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::CHANGE_ID,
        hint: "c",
        label: "change-id",
    },
    SearchScopeSpec {
        flag: SearchScopes::COMMIT_ID,
        hint: "i",
        label: "commit-id",
    },
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::BOOKMARK,
        hint: "b",
        label: "bookmark",
    },
    SearchScopeSpec {
        flag: SearchScopes::AUTHOR,
        hint: "a",
        label: "author",
    },
    SearchScopeSpec {
        flag: SearchScopes::PATH_COMMAND,
        hint: "p",
        label: "path",
    },
    SearchScopeSpec {
        flag: SearchScopes::LINE,
        hint: "l",
        label: "line",
    },
    SearchScopeSpec {
        flag: SearchScopes::TAG,
        hint: "t",
        label: "tag",
    },
];

pub const BOOKMARK_VIEW_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::BOOKMARK,
        hint: "b",
        label: "bookmark",
    },
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::CHANGE_ID,
        hint: "c",
        label: "change-id",
    },
];

pub const TAG_VIEW_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::TAG,
        hint: "t",
        label: "tag",
    },
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::CHANGE_ID,
        hint: "c",
        label: "change-id",
    },
];

pub const OP_LOG_VIEW_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::PATH_COMMAND,
        hint: "c",
        label: "command",
    },
];

pub fn scope_specs_for_view(view: crate::app::ActiveView) -> &'static [SearchScopeSpec] {
    match view {
        crate::app::ActiveView::Dag => SEARCH_SCOPE_SPECS,
        crate::app::ActiveView::Bookmarks => BOOKMARK_VIEW_SCOPE_SPECS,
        crate::app::ActiveView::Tags => TAG_VIEW_SCOPE_SPECS,
        crate::app::ActiveView::Operations => OP_LOG_VIEW_SCOPE_SPECS,
        crate::app::ActiveView::Evolog => SEARCH_SCOPE_SPECS,
        crate::app::ActiveView::Workspaces => WORKSPACE_VIEW_SCOPE_SPECS,
        crate::app::ActiveView::CommandLog => COMMAND_LOG_VIEW_SCOPE_SPECS,
    }
}

pub const WORKSPACE_VIEW_SCOPE_SPECS: &[SearchScopeSpec] = &[
    SearchScopeSpec {
        flag: SearchScopes::DESCRIPTION,
        hint: "d",
        label: "description",
    },
    SearchScopeSpec {
        flag: SearchScopes::CHANGE_ID,
        hint: "c",
        label: "change-id",
    },
];

pub const COMMAND_LOG_VIEW_SCOPE_SPECS: &[SearchScopeSpec] = &[SearchScopeSpec {
    flag: SearchScopes::DESCRIPTION,
    hint: "d",
    label: "description",
}];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Query,
    Scopes,
}

pub struct SearchState {
    pub input: Input,
    pub matches: Vec<RowIdx>,
    pub current_match: Option<usize>,
    pub restore_cursor: RowIdx,
    pub scopes: SearchScopes,
    pub focus: SearchFocus,
}

impl SearchState {
    pub fn new(restore_cursor: RowIdx) -> Self {
        Self {
            input: Input::new(String::new()),
            matches: Vec::new(),
            current_match: None,
            restore_cursor,
            scopes: SearchScopes::DEFAULT,
            focus: SearchFocus::Query,
        }
    }

    pub fn query(&self) -> &str {
        self.input.value()
    }
}
