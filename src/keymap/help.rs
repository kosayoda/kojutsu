use super::registry::{ActionId, ActionRegistry};
use super::trie::{Keymap, TrieNode};
use super::{display_key, toggle_hint, AppAction, SelectionKindSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HelpGroup {
    Commands,
    Navigation,
    General,
}

impl HelpGroup {
    pub fn label(self) -> &'static str {
        match self {
            HelpGroup::Navigation => "Navigation",
            HelpGroup::Commands => "Commands",
            HelpGroup::General => "General",
        }
    }
}

pub struct HelpEntry {
    pub keys: String,
    pub description: String,
    pub group: HelpGroup,
    pub selection_support: SelectionKindSet,
    pub requires_conflict: bool,
    pub requires_file: bool,
}

fn sort_key(keys: &str) -> (String, bool) {
    let upper = keys.starts_with(|c: char| c.is_uppercase());
    (keys.to_lowercase(), upper)
}

pub fn help_entries(
    keymap: &Keymap,
    registry: &ActionRegistry,
    presets: &[crate::theme::Preset],
) -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    let mut action_keys: Vec<(ActionId, Vec<String>, String, HelpGroup)> = Vec::new();
    let mut prefix_entries: Vec<HelpEntry> = Vec::new();

    for (node, trie_node) in &keymap.root {
        match trie_node {
            TrieNode::Action {
                id,
                description,
                group,
            } => {
                let key_str = if let ActionId::Builtin(action) = id {
                    toggle_hint(*action)
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| display_key(node))
                } else {
                    display_key(node)
                };
                if let Some(existing) = action_keys.iter_mut().find(|(a, _, _, _)| a == id) {
                    existing.1.push(key_str);
                } else {
                    action_keys.push((*id, vec![key_str], description.to_string(), *group));
                }
            }
            TrieNode::Prefix {
                label,
                group,
                children,
            } => {
                let key_str = display_key(node);
                prefix_entries.push(HelpEntry {
                    keys: format!("{key_str} \u{2026}"),
                    description: label.to_string(),
                    group: *group,
                    selection_support: children.iter().fold(
                        SelectionKindSet::empty(),
                        |acc, (_, child)| match child {
                            TrieNode::Action { id, .. } => {
                                acc | registry.selection_support(*id)
                            }
                            _ => acc,
                        },
                    ),
                    requires_conflict: children.iter().all(|(_, c)| {
                        matches!(c, TrieNode::Action { id, .. } if registry.requires_conflict(*id))
                    }),
                    requires_file: children.iter().all(|(_, c)| {
                        matches!(c, TrieNode::Action { id, .. } if registry.requires_file(*id))
                    }),
                });
            }
            TrieNode::Toggle { .. } => {}
        }
    }

    let mut entries: Vec<HelpEntry> = action_keys
        .into_iter()
        .map(|(id, keys, desc, group)| {
            let description = if let ActionId::Builtin(AppAction::SwitchPreset(idx)) = id {
                if let Some(preset) = presets.get(idx) {
                    format!("{desc} ({name})", name = preset.name)
                } else {
                    desc
                }
            } else {
                desc
            };
            HelpEntry {
                keys: keys.join(" / "),
                description,
                group,
                selection_support: registry.selection_support(id),
                requires_conflict: false,
                requires_file: false,
            }
        })
        .collect();
    entries.extend(prefix_entries);

    entries.sort_unstable_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(sort_key(&a.keys).cmp(&sort_key(&b.keys)))
    });

    let mut groups: Vec<(HelpGroup, Vec<HelpEntry>)> = Vec::new();
    for entry in entries {
        if let Some(last) = groups.last_mut() {
            if last.0 == entry.group {
                last.1.push(entry);
                continue;
            }
        }
        let group = entry.group;
        groups.push((group, vec![entry]));
    }

    groups
}

pub fn select_mode_help_entries() -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    use HelpGroup::{General as G, Navigation as N};

    fn h(keys: &str, desc: &str, group: HelpGroup) -> HelpEntry {
        HelpEntry {
            keys: keys.into(),
            description: desc.into(),
            group,
            selection_support: SelectionKindSet::ALL,
            requires_conflict: false,
            requires_file: false,
        }
    }

    let mut nav = vec![
        h("j / down", "move down", N),
        h("k / up", "move up", N),
        h("J", "next commit", N),
        h("K", "prev commit", N),
        h("ctrl-d / pagedown", "page down", N),
        h("ctrl-u / pageup", "page up", N),
        h("@", "jump to @", N),
        h("0", "go to top", N),
        h("$", "go to bottom", N),
        h("tab", "toggle fold", N),
        h("/", "search", N),
        h("ctrl-n", "next match", N),
        h("ctrl-p", "prev match", N),
    ];
    nav.sort_unstable_by(|a, b| sort_key(&a.keys).cmp(&sort_key(&b.keys)));

    let mut general = vec![
        h("Enter", "confirm selection", G),
        h("Esc", "cancel", G),
        h("?", "help", G),
    ];
    general.sort_unstable_by(|a, b| sort_key(&a.keys).cmp(&sort_key(&b.keys)));

    vec![(N, nav), (G, general)]
}
