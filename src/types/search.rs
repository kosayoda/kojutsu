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
    pub const DEFAULT_ANNOTATE: Self = Self::CHANGE_ID.union(Self::LINE);

    pub const fn spec(self) -> SearchScopeSpec {
        SearchScopeSpec {
            flag: self,
            hint: self.default_hint(),
            label: self.default_label(),
        }
    }

    const fn default_hint(self) -> &'static str {
        match self {
            Self::CHANGE_ID => "c",
            Self::COMMIT_ID => "i",
            Self::DESCRIPTION => "d",
            Self::BOOKMARK => "b",
            Self::AUTHOR => "a",
            Self::PATH_COMMAND => "p",
            Self::LINE => "l",
            Self::TAG => "t",
            _ => "",
        }
    }

    const fn default_label(self) -> &'static str {
        match self {
            Self::CHANGE_ID => "change-id",
            Self::COMMIT_ID => "commit-id",
            Self::DESCRIPTION => "description",
            Self::BOOKMARK => "bookmark",
            Self::AUTHOR => "author",
            Self::PATH_COMMAND => "path",
            Self::LINE => "line",
            Self::TAG => "tag",
            _ => "",
        }
    }
}

pub struct SearchScopeSpec {
    pub flag: SearchScopes,
    pub hint: &'static str,
    pub label: &'static str,
}

use crate::app::ActiveView;

pub fn scope_specs_for_view(view: ActiveView) -> &'static [SearchScopeSpec] {
    use SearchScopes as S;
    match view {
        ActiveView::Dag => {
            const V: &[SearchScopeSpec] = &[
                S::CHANGE_ID.spec(),
                S::COMMIT_ID.spec(),
                S::DESCRIPTION.spec(),
                S::BOOKMARK.spec(),
                S::AUTHOR.spec(),
                S::PATH_COMMAND.spec(),
                S::LINE.spec(),
                S::TAG.spec(),
            ];
            V
        }
        ActiveView::Bookmarks => {
            const V: &[SearchScopeSpec] = &[
                S::BOOKMARK.spec(),
                S::DESCRIPTION.spec(),
                S::CHANGE_ID.spec(),
            ];
            V
        }
        ActiveView::Tags => {
            const V: &[SearchScopeSpec] =
                &[S::TAG.spec(), S::DESCRIPTION.spec(), S::CHANGE_ID.spec()];
            V
        }
        ActiveView::Operations => {
            const V: &[SearchScopeSpec] = &[
                S::DESCRIPTION.spec(),
                SearchScopeSpec {
                    flag: S::PATH_COMMAND,
                    hint: "c",
                    label: "command",
                },
            ];
            V
        }
        ActiveView::Workspaces => {
            const V: &[SearchScopeSpec] = &[S::DESCRIPTION.spec(), S::CHANGE_ID.spec()];
            V
        }
        ActiveView::Evolog => {
            const V: &[SearchScopeSpec] = &[
                S::CHANGE_ID.spec(),
                S::DESCRIPTION.spec(),
                S::AUTHOR.spec(),
                S::PATH_COMMAND.spec(),
            ];
            V
        }
        ActiveView::CommandLog => {
            const V: &[SearchScopeSpec] = &[S::DESCRIPTION.spec()];
            V
        }
        ActiveView::Interdiff => {
            const V: &[SearchScopeSpec] = &[
                S::DESCRIPTION.spec(),
                S::PATH_COMMAND.spec(),
                S::LINE.spec(),
            ];
            V
        }
        ActiveView::Annotate => {
            const V: &[SearchScopeSpec] = &[
                S::CHANGE_ID.spec(),
                S::DESCRIPTION.spec(),
                S::AUTHOR.spec(),
                S::LINE.spec(),
                S::PATH_COMMAND.spec(),
            ];
            V
        }
    }
}

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
    pub focus: SearchFocus,
}

impl SearchState {
    pub fn new(restore_cursor: RowIdx) -> Self {
        Self {
            input: Input::new(String::new()),
            matches: Vec::new(),
            current_match: None,
            restore_cursor,
            focus: SearchFocus::Query,
        }
    }

    pub fn query(&self) -> &str {
        self.input.value()
    }
}
