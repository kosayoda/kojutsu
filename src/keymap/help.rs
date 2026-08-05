use super::registry::{ActionId, ActionRegistry};
use super::trie::{Keymap, TrieNode};
use super::{SelectionKindSet, display_key, toggle_hint};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[strum(ascii_case_insensitive)]
pub enum HelpGroup {
    Commands,
    Navigation,
    General,
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

    // ToggleSelect doubles as the per-hunk side picker on conflict term
    // rows; surface that contextual meaning as a conflict-gated entry
    // under whatever key the action is actually bound to.
    let pick_entry = action_keys
        .iter()
        .find(|(id, ..)| *id == ActionId::Builtin(super::AppAction::ToggleSelect))
        .map(|(_, keys, _, _)| HelpEntry {
            keys: keys.join(" / "),
            description: "pick conflict side at cursor".into(),
            group: HelpGroup::Commands,
            selection_support: SelectionKindSet::ALL,
            requires_conflict: true,
            requires_file: false,
        });

    let mut entries: Vec<HelpEntry> = action_keys
        .into_iter()
        .map(|(id, keys, desc, group)| {
            let preset_slot = match id {
                ActionId::Builtin(action) => action.preset_slot(),
                ActionId::Lua(_) => None,
            };
            let description = if let Some(preset) = preset_slot.and_then(|idx| presets.get(idx)) {
                format!("{desc} ({name})", name = preset.name)
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
    entries.extend(pick_entry);
    entries.extend(prefix_entries);

    entries.sort_unstable_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(sort_key(&a.keys).cmp(&sort_key(&b.keys)))
    });

    let mut groups: Vec<(HelpGroup, Vec<HelpEntry>)> = Vec::new();
    for entry in entries {
        if let Some(last) = groups.last_mut()
            && last.0 == entry.group
        {
            last.1.push(entry);
            continue;
        }
        let group = entry.group;
        groups.push((group, vec![entry]));
    }

    groups
}

/// Help for target- and commit-select. Read from the same keymap the modes
/// resolve against, and filtered by the same predicate, so the listing can't
/// drift from what they actually accept: only top-level bindings, since a
/// sequence needs submenu state these modes don't have.
pub fn select_mode_help_entries(keymap: &Keymap) -> Vec<(HelpGroup, Vec<HelpEntry>)> {
    use HelpGroup::General as G;

    fn h(keys: String, desc: String, group: HelpGroup) -> HelpEntry {
        HelpEntry {
            keys,
            description: desc,
            group,
            selection_support: SelectionKindSet::ALL,
            requires_conflict: false,
            requires_file: false,
        }
    }

    let mut by_action: Vec<(super::AppAction, Vec<String>, &str, HelpGroup)> = Vec::new();
    for (node, trie_node) in &keymap.root {
        let TrieNode::Action {
            id: ActionId::Builtin(action),
            description,
            group,
        } = trie_node
        else {
            continue;
        };
        if !action.is_cursor_navigation() {
            continue;
        }
        match by_action.iter_mut().find(|(a, ..)| a == action) {
            Some(entry) => entry.1.push(display_key(node)),
            None => by_action.push((*action, vec![display_key(node)], description, *group)),
        }
    }

    let mut grouped: Vec<(HelpGroup, Vec<HelpEntry>)> = Vec::new();
    let mut push =
        |group: HelpGroup, entry: HelpEntry| match grouped.iter_mut().find(|(g, _)| *g == group) {
            Some((_, entries)) => entries.push(entry),
            None => grouped.push((group, vec![entry])),
        };

    for (_, keys, description, group) in by_action {
        push(group, h(keys.join(" / "), description.to_string(), group));
    }
    // Keys the modes own outright, bound nowhere in the keymap.
    push(G, h("Enter".into(), "confirm selection".into(), G));
    push(G, h("Esc".into(), "cancel".into(), G));

    grouped.sort_unstable_by_key(|(group, _)| *group);
    for (_, entries) in &mut grouped {
        entries.sort_unstable_by_key(|e| sort_key(&e.keys));
    }
    grouped
}
