use renderdag::{Ancestor, GraphRowRenderer, Renderer};

use crate::dag::{DagEntry, EdgeKind};

/// Sentinel characters used to identify line roles in the renderer output.
/// We pass these as a 2-line "message" to the renderer, then identify which
/// output line is the node line vs continuation line by looking for the sentinel.
const NODE_SENTINEL: char = '\x01';
const CONT_SENTINEL: char = '\x02';

/// Pre-rendered graph lines for a single commit node.
///
/// Each field contains the graph prefix (glyphs + padding) already formatted
/// by the renderer to the correct column width.
pub struct GraphLines {
    /// Graph prefix for the node line (contains the glyph character).
    pub node: String,
    /// Graph prefix for the continuation/description line.
    pub cont: String,
    /// Additional graph-only lines (link, pad, term lines between commits).
    pub extra: Vec<String>,
}

/// Render the DAG graph column for all entries using `BoxDrawingRenderer`.
///
/// Returns one [`GraphLines`] per entry, in the same order as the input.
/// Each `GraphLines` contains properly-padded graph prefixes that the UI
/// can directly concatenate with styled content.
pub fn render(entries: &[DagEntry]) -> Vec<GraphLines> {
    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(2)
        .build_box_drawing();

    entries
        .iter()
        .map(|entry| {
            // Filter out Missing edges when there are reachable (Direct/Indirect)
            // edges. Missing edges only add useless ~ terminator columns on
            // merge commits whose parents are outside the revset.
            let has_reachable = entry
                .edges
                .iter()
                .any(|e| !matches!(e.kind, EdgeKind::Missing));
            let parents: Vec<Ancestor<String>> = if has_reachable {
                entry
                    .edges
                    .iter()
                    .filter(|e| !matches!(e.kind, EdgeKind::Missing))
                    .map(|e| match e.kind {
                        EdgeKind::Direct => Ancestor::Parent(e.target.to_string()),
                        EdgeKind::Indirect => Ancestor::Ancestor(e.target.to_string()),
                        EdgeKind::Missing => unreachable!(),
                    })
                    .collect()
            } else if entry.edges.is_empty() {
                vec![]
            } else {
                // All edges are Missing -- keep one for the ~ terminator.
                vec![Ancestor::Anonymous]
            };

            let glyph = entry.commit.glyph();

            // Pass a 2-line message with different sentinel characters so we
            // can identify which output line is the node line vs continuation
            // line. The renderer may insert extra pad/link/term lines and
            // prepend an extra_pad_line from the previous entry, so we can't
            // rely on positional indexing.
            let message = format!("{NODE_SENTINEL}\n{CONT_SENTINEL}");
            let row = renderer.next_row(
                entry.commit.graph_id.clone(),
                parents,
                glyph.to_string(),
                message,
            );

            let mut node = String::new();
            let mut cont = String::new();
            let mut extra = Vec::new();

            for line in row.lines() {
                if let Some(idx) = line.find(NODE_SENTINEL) {
                    // Node line: everything before the sentinel is the graph prefix
                    node = line[..idx].to_string();
                } else if let Some(idx) = line.find(CONT_SENTINEL) {
                    // Continuation line: everything before the sentinel
                    cont = line[..idx].to_string();
                } else {
                    // Pure graph line (link, pad, term, extra_pad)
                    extra.push(line.trim_end().to_string());
                }
            }

            GraphLines { node, cont, extra }
        })
        .collect()
}
