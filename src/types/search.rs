use tui_input::Input;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SearchScopes: u8 {
        const CHANGE_ID   = 1 << 0;
        const COMMIT_ID   = 1 << 1;
        const DESCRIPTION = 1 << 2;
        const BOOKMARK    = 1 << 3;
        const AUTHOR      = 1 << 4;
        const PATH        = 1 << 5;
        const LINE        = 1 << 6;
    }
}

impl SearchScopes {
    pub const DEFAULT: Self = Self::CHANGE_ID;
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
        flag: SearchScopes::PATH,
        hint: "p",
        label: "path",
    },
    SearchScopeSpec {
        flag: SearchScopes::LINE,
        hint: "l",
        label: "line",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Query,
    Scopes,
}

pub struct SearchState {
    pub input: Input,
    pub matches: Vec<usize>,
    pub current_match: Option<usize>,
    pub restore_cursor: usize,
    pub scopes: SearchScopes,
    pub focus: SearchFocus,
}

impl SearchState {
    pub fn new(restore_cursor: usize) -> Self {
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
