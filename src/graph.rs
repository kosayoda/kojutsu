use renderdag::{Ancestor, BoxDrawingRenderer, GraphRowRenderer, Renderer};

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
#[derive(Default)]
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

/// Stateful DAG graph renderer. Entries can be rendered incrementally
/// (chunk by chunk) because renderdag's column state persists across calls.
pub struct DagGraphRenderer {
    inner: BoxDrawingRenderer<String, GraphRowRenderer<String>>,
}

impl DagGraphRenderer {
    pub fn new() -> Self {
        Self {
            inner: GraphRowRenderer::new()
                .output()
                .with_min_row_height(2)
                .build_box_drawing(),
        }
    }

    /// Render graph lines for the next batch of DAG entries, continuing from
    /// any previously rendered rows.
    pub fn render(&mut self, entries: &[DagEntry], glyphs: &GlyphChars) -> Vec<GraphLines> {
        entries
            .iter()
            .map(|entry| {
                self.render_row(
                    entry.commit.graph_id.as_str(),
                    &entry.edges,
                    glyphs.char_for(entry.commit.glyph()),
                )
            })
            .collect()
    }

    /// Render the graph lines for a single row.
    fn render_row(&mut self, id: &str, edges: &[Edge], glyph: char) -> GraphLines {
        let parents: Vec<Ancestor<String>> = edges
            .iter()
            .map(|e| match e.kind {
                EdgeKind::Direct => Ancestor::Parent(e.target.to_string()),
                EdgeKind::Indirect => Ancestor::Ancestor(e.target.to_string()),
                EdgeKind::Missing => Ancestor::Anonymous,
            })
            .collect();

        let message = format!("{NODE_SENTINEL}\n{CONT_SENTINEL}\n{REST_SENTINEL}");
        let row = self
            .inner
            .next_row(id.to_string(), parents, glyph.to_string(), message);

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
    }
}

impl Default for DagGraphRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Render graph lines for a list of entries with edges and a glyph per entry.
///
/// Generic over the entry type: callers provide ID, edges, and glyph for each.
pub fn render_generic(entries: &[(&str, &[Edge], char)]) -> Vec<GraphLines> {
    let mut renderer = DagGraphRenderer::new();
    entries
        .iter()
        .map(|(id, edges, glyph)| renderer.render_row(id, edges, *glyph))
        .collect()
}
