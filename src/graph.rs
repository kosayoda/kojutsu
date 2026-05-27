use renderdag::{Ancestor, GraphRowRenderer, Renderer};

use crate::dag::{DagEntry, Edge, EdgeKind};
use crate::theme::GlyphChars;

/// Sentinel characters used to identify line roles in the renderer output.
/// We pass these as a 3-line "message" to the renderer, then identify which
/// output line is the node line, first continuation, or rest continuation
/// by looking for the sentinel.
const NODE_SENTINEL: char = '\x01';
const CONT_SENTINEL: char = '\x02';
const REST_SENTINEL: char = '\x03';

/// Pre-rendered graph lines for a single commit node.
///
/// Each field contains the graph prefix (glyphs + padding) already formatted
/// by the renderer to the correct column width.
pub struct GraphLines {
    /// Graph prefix for the node line (contains the glyph character).
    pub node: String,
    /// Graph prefix for the first continuation line (may contain merge connectors).
    pub cont: String,
    /// Graph prefix for subsequent continuation lines (plain vertical bars).
    pub rest: String,
    /// Additional graph-only lines (link, pad, term lines between commits).
    pub extra: Vec<String>,
}

/// Render the DAG graph column for all entries using `BoxDrawingRenderer`.
///
/// Returns one [`GraphLines`] per entry, in the same order as the input.
/// Each `GraphLines` contains properly-padded graph prefixes that the UI
/// can directly concatenate with styled content.
pub fn render(entries: &[DagEntry], glyphs: &GlyphChars) -> Vec<GraphLines> {
    let ids: Vec<String> = entries
        .iter()
        .map(|e| e.commit.graph_id.to_string())
        .collect();
    let generic: Vec<(&str, &[Edge], char)> = entries
        .iter()
        .zip(&ids)
        .map(|(entry, id)| {
            (
                id.as_str(),
                entry.edges.as_slice(),
                glyphs.char_for(entry.commit.glyph()),
            )
        })
        .collect();
    render_generic(&generic)
}

/// Render graph lines for a list of entries with edges and a glyph per entry.
///
/// Generic over the entry type — callers provide ID, edges, and glyph for each.
pub fn render_generic(entries: &[(&str, &[Edge], char)]) -> Vec<GraphLines> {
    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(2)
        .build_box_drawing();

    entries
        .iter()
        .map(|(id, edges, glyph)| {
            let parents: Vec<Ancestor<String>> = if edges.is_empty() {
                vec![]
            } else {
                edges
                    .iter()
                    .map(|e| match e.kind {
                        EdgeKind::Direct => Ancestor::Parent(e.target.to_string()),
                        EdgeKind::Indirect => Ancestor::Ancestor(e.target.to_string()),
                        EdgeKind::Missing => Ancestor::Anonymous,
                    })
                    .collect()
            };

            let message = format!("{NODE_SENTINEL}\n{CONT_SENTINEL}\n{REST_SENTINEL}");
            let row = renderer.next_row(id.to_string(), parents, glyph.to_string(), message);

            let mut node = String::new();
            let mut cont = String::new();
            let mut rest = String::new();
            let mut extra = Vec::new();

            for line in row.lines() {
                if let Some(idx) = line.find(NODE_SENTINEL) {
                    node = line[..idx].to_string();
                } else if let Some(idx) = line.find(CONT_SENTINEL) {
                    cont = line[..idx].to_string();
                } else if let Some(idx) = line.find(REST_SENTINEL) {
                    rest = line[..idx].to_string();
                } else {
                    extra.push(line.trim_end().to_string());
                }
            }

            GraphLines {
                node,
                cont,
                rest,
                extra,
            }
        })
        .collect()
}
