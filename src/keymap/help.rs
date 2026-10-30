use super::registry::{ActionId, ActionRegistry, Gate};
use super::trie::{Keymap, TrieNode};
use super::{Requires, SelectionKindSet, display_key};
use crate::jj_version::JjFeature;
use keymap_parser::Node;

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
    pub gate: Gate,
}

fn sort_key(keys: &str) -> (String, bool) {
    let upper = keys.starts_with(|c: char| c.is_uppercase());
    (keys.to_lowercase(), upper)
}

/// What a prefix needs to be available, so it greys out only when none of
/// its entries could run: any selection one of its actions takes, a context
/// they all read, and the oldest jj feature, when every one of them needs
/// one. Toggles run nothing, so they are no entry; a nested prefix might hold
/// anything, so it holds nothing back.
fn prefix_gate(registry: &ActionRegistry, children: &[(Node, TrieNode)]) -> Gate {
    let gates: Vec<Gate> = children
        .iter()
        .filter_map(|(_, child)| match child {
            TrieNode::Action { id, .. } => Some(registry.gate(*id)),
            TrieNode::Prefix { .. } => Some(Gate {
                selection: SelectionKindSet::empty(),
                ..Gate::OPEN
            }),
            TrieNode::Toggle { .. } => None,
        })
        .collect();
    let selection = gates
        .iter()
        .fold(SelectionKindSet::empty(), |acc, gate| acc | gate.selection);
    let requires = match gates.split_first() {
        Some((first, rest)) if rest.iter().all(|g| g.requires == first.requires) => first.requires,
        _ => Requires::Nothing,
    };
    let jj = gates
        .iter()
        .map(|gate| gate.jj)
        .collect::<Option<Vec<JjFeature>>>()
        .and_then(|features| features.into_iter().min_by_key(|f| f.since()));
    Gate {
        selection,
        requires,
        jj,
    }
}

pub fn help_entries(
    keymap: &Keymap,
    registry: &ActionRegistry,
    presets: &[crate::config::Preset],
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
                let key_str = display_key(node);
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
                    gate: prefix_gate(registry, children),
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
            gate: Gate {
                requires: Requires::Conflict,
                ..Gate::OPEN
            },
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
                gate: registry.gate(id),
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
            gate: Gate::OPEN,
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
        if action.spec().effect != crate::keymap::Effect::Navigate {
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

#[cfg(test)]
mod prefix_gate_tests {
    use super::*;
    use crate::jj_version::{InstalledJj, JjVersion};
    use crate::keymap::{Availability, Keymaps, default_bindings};
    use crate::types::ActiveView;

    /// Whether the DAG view's help greys out the prefix on `key` on jj
    /// `minor`.
    fn prefix_blocked(key: &str, minor: u32) -> bool {
        let keymaps = Keymaps::build(default_bindings(), ActionRegistry::new());
        let groups = help_entries(keymaps.for_view(ActiveView::Dag), &keymaps.registry, &[]);
        let entry = groups
            .iter()
            .flat_map(|(_, entries)| entries)
            .find(|entry| entry.keys == format!("{key} \u{2026}"))
            .unwrap_or_else(|| panic!("no prefix on {key}"));
        Availability {
            selection: SelectionKindSet::empty(),
            on_file: false,
            on_conflict: false,
            jj: InstalledJj::known(JjVersion::new(0, minor, 0)),
        }
        .blocks(entry.gate)
    }

    /// A prefix greys out once every action under it is too new: its toggles
    /// run nothing, so they don't keep it lit.
    #[test]
    fn a_prefix_is_blocked_when_all_it_runs_is_too_new() {
        assert!(prefix_blocked("!", 42));
        assert!(!prefix_blocked("!", 43));
        assert!(prefix_blocked("m", 44));
        assert!(!prefix_blocked("m", 45));
    }

    /// One action jj can run keeps the prefix available: tag set is old,
    /// tag track is not.
    #[test]
    fn a_prefix_with_anything_runnable_stays_available() {
        assert!(!prefix_blocked("t", 36));
    }
}

#[cfg(test)]
mod help_entries_tests {
    use super::*;
    use crate::config::Preset;
    use crate::keymap::{
        AppAction, BindTarget, BindingSpec, CommandFlags, Keymaps, Scope, parse_sequence,
    };
    use crate::types::ActiveView;

    fn bind(seq: &str, action: AppAction, description: &str) -> BindingSpec {
        BindingSpec {
            keys: parse_sequence(seq).expect("valid key sequence"),
            target: BindTarget::Action {
                id: ActionId::Builtin(action),
                description: description.into(),
                group: HelpGroup::Commands,
            },
            scope: Scope::All,
        }
    }

    fn entries(specs: Vec<BindingSpec>, presets: &[Preset]) -> Vec<HelpEntry> {
        let keymaps = Keymaps::build(specs, ActionRegistry::new());
        help_entries(
            keymaps.for_view(ActiveView::Dag),
            &keymaps.registry,
            presets,
        )
        .into_iter()
        .flat_map(|(_, entries)| entries)
        .collect()
    }

    /// The help for `specs`, as each entry's keys and description in order.
    fn listing(specs: Vec<BindingSpec>) -> Vec<(String, String)> {
        entries(specs, &[])
            .into_iter()
            .map(|entry| (entry.keys, entry.description))
            .collect()
    }

    /// A toggle's status-bar badge letter is no key: rebound, help shows
    /// the key that toggles it.
    #[test]
    fn a_rebound_toggle_lists_the_key_it_is_bound_to() {
        let listed = listing(vec![bind(
            "ctrl-x",
            AppAction::ToggleIgnoreImmutable,
            "toggle ignore-immutable",
        )]);
        assert_eq!(
            listed,
            [("ctrl-x".into(), "toggle ignore-immutable".into())]
        );
    }

    #[test]
    fn an_action_on_several_keys_is_one_entry() {
        let listed = listing(vec![
            bind("j", AppAction::MoveDown, "move down"),
            bind("down", AppAction::MoveDown, "move down"),
        ]);
        assert_eq!(listed, [("j / down".into(), "move down".into())]);
    }

    #[test]
    fn a_capital_sorts_right_after_its_lowercase() {
        let listed = listing(vec![
            bind("b", AppAction::Squash, "squash"),
            bind("shift-a", AppAction::Abandon, "abandon"),
            bind("a", AppAction::Absorb, "absorb"),
        ]);
        let keys: Vec<&str> = listed.iter().map(|(keys, _)| keys.as_str()).collect();
        assert_eq!(keys, ["a", "A", "b"]);
    }

    /// A slot with no preset configured keeps its plain description.
    #[test]
    fn a_switch_preset_entry_names_its_preset() {
        let presets = ["mine", "all"].map(|name| Preset {
            name: name.into(),
            revset: format!("{name}()"),
        });
        let listed: Vec<String> = entries(
            vec![
                bind("!", AppAction::SwitchPreset2, "preset 2"),
                bind("#", AppAction::SwitchPreset3, "preset 3"),
            ],
            &presets,
        )
        .into_iter()
        .map(|entry| entry.description)
        .collect();
        assert_eq!(listed, ["preset 2 (all)", "preset 3"]);
    }

    /// Toggle-select picks a conflict side on a conflict row, so help lists
    /// that too, under whatever key toggle-select is bound to.
    #[test]
    fn the_conflict_pick_entry_follows_toggle_selects_key() {
        let listed = entries(
            vec![bind("x", AppAction::ToggleSelect, "toggle select")],
            &[],
        );
        let pick = listed
            .iter()
            .find(|entry| entry.description == "pick conflict side at cursor")
            .expect("a conflict pick entry");
        assert_eq!(pick.keys, "x");
        assert_eq!(pick.gate.requires, Requires::Conflict);
    }

    /// A prefix needs the cursor somewhere only when everything it runs
    /// does. Toggles run nothing and don't count; a nested prefix might
    /// hold anything.
    #[test]
    fn a_prefix_requires_a_context_only_when_all_it_runs_agrees() {
        let pick_ours = || bind("p o", AppAction::ConflictPickOurs, "take ours");
        let cases: [(BindingSpec, Requires); 4] = [
            (
                bind("p t", AppAction::ConflictPickTheirs, "take theirs"),
                Requires::Conflict,
            ),
            (bind("p s", AppAction::Squash, "squash"), Requires::Nothing),
            (
                BindingSpec {
                    keys: parse_sequence("p d").expect("valid key sequence"),
                    target: BindTarget::Toggle {
                        flag: CommandFlags::DEBUG,
                        description: "debug".into(),
                        jj: None,
                    },
                    scope: Scope::All,
                },
                Requires::Conflict,
            ),
            (
                bind("p n t", AppAction::ConflictPickTheirs, "take theirs"),
                Requires::Nothing,
            ),
        ];
        for (case, (sibling, requires)) in cases.into_iter().enumerate() {
            let listed = entries(vec![pick_ours(), sibling], &[]);
            let prefix = listed
                .iter()
                .find(|entry| entry.keys == "p \u{2026}")
                .expect("the prefix entry");
            assert_eq!(prefix.gate.requires, requires, "case {case}");
        }
    }
}
