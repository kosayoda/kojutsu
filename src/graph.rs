use renderdag::{Ancestor, GraphRowRenderer, Renderer};

use crate::dag::{DagEntry, EdgeKind};

/// Pre-rendered graph lines for a single commit node.
///
/// The first line is the node line (contains the glyph), subsequent lines
/// are link/pad lines connecting to the next node.
pub struct GraphLines {
    pub lines: Vec<String>,
}

/// Render the DAG graph column for all entries using `BoxDrawingRenderer`.
///
/// Returns one [`GraphLines`] per entry, in the same order as the input.
/// Each `GraphLines` contains the multi-line graph prefix for that commit.
pub fn render(entries: &[DagEntry]) -> Vec<GraphLines> {
    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(2)
        .build_box_drawing();

    entries
        .iter()
        .map(|entry| {
            let parents: Vec<Ancestor<String>> = entry
                .edges
                .iter()
                .map(|e| match e.kind {
                    EdgeKind::Direct => Ancestor::Parent(e.target.clone()),
                    EdgeKind::Indirect => Ancestor::Ancestor(e.target.clone()),
                    EdgeKind::Missing => Ancestor::Anonymous,
                })
                .collect();

            let glyph = entry.commit.glyph();

            // Pass the full commit ID as the node identifier (used by the
            // renderer to track column positions) and an empty message so the
            // output contains only graph characters.
            let row = renderer.next_row(
                entry.commit.graph_id.clone(),
                parents,
                glyph.to_string(),
                String::new(),
            );

            // The renderer returns a multi-line string. Split into individual
            // lines, stripping trailing whitespace.
            let lines: Vec<String> = row
                .lines()
                .map(|l: &str| l.trim_end().to_string())
                .collect();

            GraphLines {
                lines: if lines.is_empty() {
                    vec![String::new()]
                } else {
                    lines
                },
            }
        })
        .collect()
}
