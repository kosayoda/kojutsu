use std::sync::Arc;

use compact_str::CompactString;
use keymap_parser::Node;

use super::bindings::{BindTarget, BindingSpec, Scope};
use super::registry::{ActionId, ActionRegistry};
use super::{CommandFlags, HelpGroup};
use crate::app::ActiveView;

#[derive(Clone)]
pub enum TrieNode {
    Action {
        id: ActionId,
        description: CompactString,
        group: HelpGroup,
    },
    Prefix {
        label: CompactString,
        group: HelpGroup,
        children: Arc<[(Node, TrieNode)]>,
    },
    Toggle {
        flag: CommandFlags,
        description: CompactString,
    },
}

pub struct Keymap {
    pub root: Vec<(Node, TrieNode)>,
}

pub enum LookupResult {
    Action(ActionId),
    Prefix {
        label: CompactString,
        children: Arc<[(Node, TrieNode)]>,
    },
    Toggle(CommandFlags),
    Unbound,
}

impl Keymap {
    pub fn lookup(&self, key: &Node) -> LookupResult {
        Self::lookup_in(&self.root, key)
    }

    pub fn lookup_in(entries: &[(Node, TrieNode)], key: &Node) -> LookupResult {
        for (k, node) in entries {
            if k == key {
                return match node {
                    TrieNode::Action { id, .. } => LookupResult::Action(*id),
                    TrieNode::Prefix {
                        label, children, ..
                    } => LookupResult::Prefix {
                        label: label.clone(),
                        children: children.clone(),
                    },
                    TrieNode::Toggle { flag, .. } => LookupResult::Toggle(*flag),
                };
            }
        }
        LookupResult::Unbound
    }
}

pub struct Keymaps {
    views: [Keymap; <ActiveView as strum::EnumCount>::COUNT],
    pub registry: ActionRegistry,
}

impl Keymaps {
    pub fn for_view(&self, view: ActiveView) -> &Keymap {
        &self.views[view.idx()]
    }

    pub fn build(specs: Vec<BindingSpec>, registry: ActionRegistry) -> Self {
        let mut per_view: Vec<Vec<&BindingSpec>> =
            vec![Vec::new(); <ActiveView as strum::EnumCount>::COUNT];

        for spec in &specs {
            match &spec.scope {
                Scope::All => {
                    for pv in &mut per_view {
                        pv.push(spec);
                    }
                }
                Scope::Views(views) => {
                    for v in views {
                        per_view[v.idx()].push(spec);
                    }
                }
            }
        }

        let build_keymap = |view_specs: &[&BindingSpec]| -> Keymap {
            let mut root: Vec<(Node, MutableTrieNode)> = Vec::new();
            for spec in view_specs {
                insert_binding(&mut root, &spec.keys, &spec.target);
            }
            Keymap {
                root: freeze_nodes(root),
            }
        };

        let views = std::array::from_fn(|i| build_keymap(&per_view[i]));
        Keymaps { views, registry }
    }
}

enum MutableTrieNode {
    Action {
        id: ActionId,
        description: CompactString,
        group: HelpGroup,
    },
    Prefix {
        label: CompactString,
        group: HelpGroup,
        children: Vec<(Node, MutableTrieNode)>,
    },
    Toggle {
        flag: CommandFlags,
        description: CompactString,
    },
}

fn insert_binding(nodes: &mut Vec<(Node, MutableTrieNode)>, keys: &[Node], target: &BindTarget) {
    assert!(!keys.is_empty(), "binding spec with empty key sequence");

    if keys.len() == 1 {
        let key = keys[0].clone();
        match target {
            BindTarget::Action {
                id,
                description,
                group,
            } => {
                // Remove any existing binding at this key (override).
                nodes.retain(|(k, _)| k != &key);
                nodes.push((
                    key,
                    MutableTrieNode::Action {
                        id: *id,
                        description: description.clone(),
                        group: *group,
                    },
                ));
            }
            BindTarget::Prefix { label, group } => {
                let existing = nodes.iter_mut().find_map(|(k, n)| match n {
                    MutableTrieNode::Prefix {
                        label: l, group: g, ..
                    } if *k == key => Some((l, g)),
                    _ => None,
                });
                if let Some((l, g)) = existing {
                    *l = label.clone();
                    *g = *group;
                } else {
                    nodes.retain(|(k, _)| k != &key);
                    nodes.push((
                        key,
                        MutableTrieNode::Prefix {
                            label: label.clone(),
                            group: *group,
                            children: Vec::new(),
                        },
                    ));
                }
            }
            BindTarget::Toggle { flag, description } => {
                nodes.retain(|(k, _)| k != &key);
                nodes.push((
                    key,
                    MutableTrieNode::Toggle {
                        flag: *flag,
                        description: description.clone(),
                    },
                ));
            }
            BindTarget::Unbind => {
                nodes.retain(|(k, _)| k != &key);
            }
        }
    } else {
        // Multi-key: first key must be or become a prefix.
        let prefix_key = keys[0].clone();
        let rest = &keys[1..];

        // Find or create the prefix node.
        let prefix_node = nodes.iter_mut().find_map(|(k, n)| {
            if k == &prefix_key
                && let MutableTrieNode::Prefix { children, .. } = n
            {
                return Some(children);
            }
            None
        });

        if let Some(children) = prefix_node {
            insert_binding(children, rest, target);
        } else {
            // Auto-create the prefix (with empty label — will be overridden
            // if an explicit Prefix spec exists).
            let mut children = Vec::new();
            insert_binding(&mut children, rest, target);
            nodes.push((
                prefix_key,
                MutableTrieNode::Prefix {
                    label: CompactString::default(),
                    group: HelpGroup::Commands,
                    children,
                },
            ));
        }
    }
}

fn freeze_nodes(nodes: Vec<(Node, MutableTrieNode)>) -> Vec<(Node, TrieNode)> {
    nodes
        .into_iter()
        .map(|(key, node)| {
            let frozen = match node {
                MutableTrieNode::Action {
                    id,
                    description,
                    group,
                } => TrieNode::Action {
                    id,
                    description,
                    group,
                },
                MutableTrieNode::Prefix {
                    label,
                    group,
                    children,
                } => TrieNode::Prefix {
                    label,
                    group,
                    children: freeze_nodes(children).into(),
                },
                MutableTrieNode::Toggle { flag, description } => {
                    TrieNode::Toggle { flag, description }
                }
            };
            (key, frozen)
        })
        .collect()
}
