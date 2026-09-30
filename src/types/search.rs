use tui_input::Input;

use super::DisplayRow;
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

use crate::types::ActiveView;

/// The scopes a view can actually search, i.e. the ones it offers as
/// toggles. Defaults are intersected with this so a view can never start out
/// filtering on something it neither displays nor matches against.
pub fn available_scopes(view: ActiveView) -> SearchScopes {
    scope_specs_for_view(view)
        .iter()
        .fold(SearchScopes::empty(), |acc, spec| acc | spec.flag)
}

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
    /// Where the cursor was when the search began: where cancelling returns
    /// it, and where the first match is looked for from. Kept as the row,
    /// since rows can come and go above it while the query is typed.
    pub started_on: Option<DisplayRow>,
    pub focus: SearchFocus,
}

impl SearchState {
    pub fn new(started_on: Option<DisplayRow>) -> Self {
        Self {
            input: Input::new(String::new()),
            matches: Vec::new(),
            current_match: None,
            started_on,
            focus: SearchFocus::Query,
        }
    }

    pub fn query(&self) -> &str {
        self.input.value()
    }
}

#[cfg(test)]
mod scope_tests {
    use super::{SearchScopes, available_scopes, scope_specs_for_view};
    use crate::types::ActiveView;
    use strum::IntoEnumIterator as _;

    /// A scope a view starts with but can't display or toggle is an invisible
    /// filter. CommandLog and Interdiff used to inherit CHANGE_ID that way.
    #[test]
    fn no_view_starts_with_a_scope_it_cannot_offer() {
        for view in ActiveView::iter() {
            let defaults = view.default_scopes(SearchScopes::DEFAULT);
            let available = available_scopes(view);
            assert!(
                available.contains(defaults),
                "{view} defaults to {:?}, which is outside {:?}",
                defaults,
                available
            );
            assert!(!defaults.is_empty(), "{view} would start unable to match");
        }
    }

    /// Configuring scopes a view has no use for must not leave it unable to
    /// search at all.
    #[test]
    fn an_unusable_configuration_falls_back_to_what_the_view_offers() {
        let only_commit_id = SearchScopes::COMMIT_ID;
        // The command log offers description only.
        let scopes = ActiveView::CommandLog.default_scopes(only_commit_id);
        assert_eq!(scopes, available_scopes(ActiveView::CommandLog));
        assert!(!scopes.is_empty());
    }

    /// The policy, stated as sensitivity to the configuration rather than by
    /// quoting each view's constant: a generic view tracks what's configured,
    /// a tailored one is unmoved by it.
    #[test]
    fn only_the_generic_views_follow_the_configuration() {
        let one = SearchScopes::CHANGE_ID | SearchScopes::AUTHOR;
        let other = SearchScopes::COMMIT_ID | SearchScopes::DESCRIPTION;

        for view in [ActiveView::Dag, ActiveView::Evolog] {
            assert_ne!(
                view.default_scopes(one),
                view.default_scopes(other),
                "{view} ignores the configured default"
            );
        }
        for view in [
            ActiveView::Bookmarks,
            ActiveView::Tags,
            ActiveView::Annotate,
        ] {
            assert_eq!(
                view.default_scopes(one),
                view.default_scopes(other),
                "{view} should keep its own default"
            );
        }
    }

    #[test]
    fn every_offered_scope_has_a_hint_and_a_label() {
        for view in ActiveView::iter() {
            for spec in scope_specs_for_view(view) {
                assert!(!spec.hint.is_empty(), "{view}: {:?} has no hint", spec.flag);
                assert!(
                    !spec.label.is_empty(),
                    "{view}: {:?} has no label",
                    spec.flag
                );
            }
        }
    }
}
