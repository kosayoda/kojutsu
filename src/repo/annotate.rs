use std::collections::HashMap;

use color_eyre::Result;
use color_eyre::eyre::Context;
use jj_lib::backend::CommitId as BackendCommitId;
use jj_lib::commit::Commit;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::Repo;
use jj_lib::repo_path::RepoPathUiConverter;
use jj_lib::revset::RevsetExtensions;
use pollster::FutureExt as _;

use super::JjRepo;
use super::operations::{format_absolute_time, millis_to_relative_time};
use crate::dag::ShortId;
use crate::types::{CommitId as UiCommitId, RepoPath};

impl JjRepo {
    /// List all file paths at a specific commit.
    pub fn list_files(&self, commit_id: &UiCommitId) -> Result<Vec<RepoPath>> {
        let repo = self.repo.as_ref();
        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;
        let commit = repo.store().get_commit(&backend_id)?;
        let tree = commit.tree();
        let mut paths = Vec::new();
        for (path, _value) in tree.entries() {
            paths.push(RepoPath::new(path.as_internal_file_string()));
        }
        Ok(paths)
    }

    pub fn file_annotate(
        &self,
        commit_id: &UiCommitId,
        file_path: &RepoPath,
    ) -> Result<crate::dag::AnnotateResult> {
        use jj_lib::annotate::FileAnnotator;
        use jj_lib::repo_path::RepoPathBuf;
        use jj_lib::revset::ResolvedRevsetExpression;

        let repo = self.repo.as_ref();

        let backend_id = BackendCommitId::try_from_hex(commit_id.as_str())
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid commit id hex"))?;
        let commit = repo.store().get_commit(&backend_id)?;

        let repo_path = RepoPathBuf::from_internal_string(file_path.as_str())
            .map_err(|e| color_eyre::eyre::eyre!("invalid repo path: {e}"))?;

        let mut annotator = FileAnnotator::from_commit(&commit, &repo_path)
            .block_on()
            .wrap_err("failed to initialize annotator")?;

        let domain = ResolvedRevsetExpression::all();
        annotator
            .compute(repo, &domain)
            .block_on()
            .map_err(|e| color_eyre::eyre::eyre!("annotation failed: {e}"))?;

        let annotation = annotator.to_annotation();

        // Build ID prefix context for short change IDs.
        let extensions = RevsetExtensions::default();
        let fileset_aliases_map = FilesetAliasesMap::new();
        let path_converter = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = self.revset_parse_context(&extensions, &fileset_aliases_map, &path_converter);
        let id_prefix_context = self.build_id_prefix_context(&context)?;
        let prefix_index = id_prefix_context
            .populate(repo)
            .wrap_err("failed to populate ID prefix index for annotate")?;

        // Cache commit metadata per unique backend CommitId.
        struct CachedMeta {
            ui_commit_id: UiCommitId,
            change_id: ShortId,
            author_display: String,
            relative_time: crate::types::Str,
        }
        let mut commit_cache: HashMap<BackendCommitId, CachedMeta> = HashMap::new();
        let mut commit_info: HashMap<UiCommitId, crate::dag::AnnotateCommitInfo> = HashMap::new();

        // Pre-seed with the annotated-at commit so header info is always available.
        {
            let change_id = Self::short_change_id(&prefix_index, repo, &commit);
            let short_cid = Self::short_commit_id(&prefix_index, repo, &backend_id);
            commit_info.insert(
                commit_id.clone(),
                build_annotate_commit_info(&commit, short_cid, change_id),
            );
        }

        let mut lines = Vec::new();
        for (line_number, (origin_result, content)) in annotation.line_origins().enumerate() {
            let (origin, outside_domain) = match origin_result {
                Ok(o) => (o, false),
                Err(o) => (o, true),
            };

            if !commit_cache.contains_key(&origin.commit_id) {
                let c = repo.store().get_commit(&origin.commit_id)?;
                let change_id = Self::short_change_id(&prefix_index, repo, &c);
                let short_commit_id = Self::short_commit_id(&prefix_index, repo, &origin.commit_id);
                let commit_id_hex = origin.commit_id.hex();

                let author_sig = c.author();
                let author_display = if author_sig.name.is_empty() {
                    author_sig.email.clone()
                } else {
                    author_sig.name.clone()
                };
                let relative_time = millis_to_relative_time(author_sig.timestamp.timestamp.0);

                let ui_cid = UiCommitId::new(&commit_id_hex);

                commit_info.insert(
                    ui_cid.clone(),
                    build_annotate_commit_info(&c, short_commit_id, change_id.clone()),
                );

                commit_cache.insert(
                    origin.commit_id.clone(),
                    CachedMeta {
                        ui_commit_id: ui_cid,
                        change_id,
                        author_display,
                        relative_time,
                    },
                );
            }
            let meta = &commit_cache[&origin.commit_id];

            let content_str = String::from_utf8_lossy(content)
                .trim_end_matches('\n')
                .to_string();

            lines.push(crate::dag::AnnotateLineData {
                commit_id: meta.ui_commit_id.clone(),
                change_id: meta.change_id.clone(),
                author: meta.author_display.clone(),
                relative_time: meta.relative_time.clone(),
                line_number: line_number + 1,
                content: content_str,
                syntax_tokens: Vec::new(),
                outside_domain,
            });
        }

        syntax_highlight_lines(&mut lines, file_path.as_str());

        Ok(crate::dag::AnnotateResult { lines, commit_info })
    }
}

fn build_annotate_commit_info(
    commit: &Commit,
    short_commit_id: ShortId,
    change_id: ShortId,
) -> crate::dag::AnnotateCommitInfo {
    let author_sig = commit.author();
    let committer_sig = commit.committer();
    crate::dag::AnnotateCommitInfo {
        commit_id: short_commit_id,
        change_id,
        author_name: author_sig.name.clone(),
        author_email: author_sig.email.clone(),
        author_date: format_absolute_time(&author_sig.timestamp),
        committer_name: committer_sig.name.clone(),
        committer_email: committer_sig.email.clone(),
        committer_date: format_absolute_time(&committer_sig.timestamp),
        description_lines: {
            let trimmed = commit.description().trim();
            if trimmed.is_empty() {
                Vec::new()
            } else {
                trimmed.lines().map(String::from).collect()
            }
        },
    }
}

/// Cached syntax definitions and ANSI theme for syntax highlighting.
fn syntax_assets() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::Theme) {
    use std::sync::LazyLock;
    use syntect::highlighting::Color;
    static ASSETS: LazyLock<(syntect::parsing::SyntaxSet, syntect::highlighting::Theme)> =
        LazyLock::new(|| {
            let ss = two_face::syntax::extra_newlines();
            let ansi = |idx: u8| Color {
                r: idx,
                g: 0,
                b: 1,
                a: 0xFF,
            };
            let theme = build_ansi_theme(ansi);
            (ss, theme)
        });
    &ASSETS
}

/// Apply syntax highlighting to annotate lines based on file extension.
/// Uses ANSI terminal colors so highlighting respects the user's color scheme.
fn syntax_highlight_lines(lines: &mut [crate::dag::AnnotateLineData], file_path: &str) {
    use syntect::easy::HighlightLines;

    let (ss, theme) = syntax_assets();

    let syntax = file_path
        .rsplit('.')
        .next()
        .and_then(|ext| ss.find_syntax_by_extension(ext))
        .or_else(|| {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            ss.find_syntax_by_extension(name)
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    if syntax.name == "Plain Text" {
        return;
    }

    let mut h = HighlightLines::new(syntax, theme);

    for line in lines.iter_mut() {
        let input = format!("{}\n", line.content);
        let Ok(regions) = h.highlight_line(&input, ss) else {
            continue;
        };
        let mut tokens = Vec::new();
        for (style, text) in regions {
            let trimmed = text.trim_end_matches('\n');
            if trimmed.is_empty() {
                continue;
            }
            let fg = style.foreground;
            let color_idx = if fg.g == 0 && fg.b == 1 {
                fg.r // decode our sentinel
            } else {
                7 // default: ANSI white
            };
            tokens.push(crate::dag::SyntaxToken {
                text: trimmed.to_string(),
                color_idx,
            });
        }
        line.syntax_tokens = tokens;
    }
}

/// Build a syntect Theme that maps syntax scopes to ANSI terminal color indices.
/// Color mapping follows the koda colorscheme conventions.
fn build_ansi_theme(
    ansi: impl Fn(u8) -> syntect::highlighting::Color,
) -> syntect::highlighting::Theme {
    use std::str::FromStr;
    use syntect::highlighting::{ScopeSelectors, Theme, ThemeItem, ThemeSettings};

    let item = |scope: &str, color_idx: u8| ThemeItem {
        scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
        style: syntect::highlighting::StyleModifier {
            foreground: Some(ansi(color_idx)),
            background: None,
            font_style: None,
        },
    };

    // ANSI indices: 1=red, 2=green, 3=yellow, 4=blue, 5=magenta, 6=cyan,
    //              7=white(fg), 8=bright black(dim)
    Theme {
        name: Some("ansi".to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(ansi(7)),
            background: Some(ansi(0)),
            ..Default::default()
        },
        scopes: vec![
            // Comments (including delimiters like ///) → dim (fg_alt)
            item("comment", 8),
            item("punctuation.definition.comment", 8),
            // Strings, characters → green (including quote delimiters)
            item("string", 2),
            item("constant.character", 2),
            item("punctuation.definition.string", 2),
            // Numbers, booleans, floats, special chars → yellow (orange equivalent)
            item("constant.numeric", 3),
            item("constant.character.escape", 3),
            item("constant.other.placeholder", 3),
            // Language constants (true/false/nil) → cyan
            item("constant.language", 6),
            item("constant", 6),
            item("entity.name.constant", 6),
            // Keywords, control flow, storage → red
            item("keyword", 1),
            item("storage", 1),
            item("keyword.control.import", 1),
            // Repeat keywords (for/while/loop) → magenta
            item("keyword.control.repeat", 5),
            // Types, structures, traits, interfaces → blue
            item("entity.name.type", 4),
            item("entity.name.class", 4),
            item("entity.name.struct", 4),
            item("entity.name.enum", 4),
            item("entity.name.union", 4),
            item("entity.name.trait", 4),
            item("entity.name.impl", 4),
            item("entity.name.interface", 4),
            item("entity.other.inherited-class", 4),
            item("support.type", 4),
            item("support.class", 4),
            // Functions → yellow
            item("entity.name.function", 3),
            item("support.function", 3),
            item("variable.function", 3),
            // Macros → magenta (override function parent)
            item("entity.name.function.macro", 5),
            item("support.macro", 5),
            item("entity.name.macro", 5),
            item("meta.attribute", 5),
            // Variables, identifiers → default fg
            item("variable", 7),
            // Operators, delimiters, punctuation → default fg
            item("keyword.operator", 7),
            item("punctuation", 7),
            // Properties, members, labels, attributes → magenta (purple equivalent)
            item("variable.other.member", 5),
            item("variable.other.property", 5),
            item("entity.name.label", 5),
            item("entity.other.attribute-name", 5),
            item("variable.annotation", 5),
            // Tags (HTML/XML) → red
            item("entity.name.tag", 1),
            // Modules, namespaces → default fg
            item("entity.name.module, entity.name.namespace", 7),
            // Language builtins (self, super, etc.) → cyan
            item("variable.language", 6),
            item("support.constant", 6),
            // Invalid → red
            item("invalid", 1),
            // Markup (markdown, etc.)
            item("markup.heading, entity.name.section", 4),
            item("markup.list", 1),
            item("markup.raw", 2),
            item("markup.underline.link", 6),
            item("markup.link", 6),
            item("markup.quote", 8),
            item("markup.inserted", 2),
            item("markup.deleted", 1),
            item("markup.changed", 3),
        ],
    }
}
