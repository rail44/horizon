//! The question: can a guest hold the document itself? A blitz-dom
//! document of rows is built, styled, laid out and painted into drawing
//! commands, all in this program, and the time of each is printed. Built
//! for `wasm32-wasip2` it is the guest of the third model; built for the
//! host it is the same work done natively, to compare.
//!
//! Arguments: the number of rows, then font files to register (a wasm
//! guest finds no fonts of its own).

use std::sync::Arc;
use std::time::Instant;

use anyrender::PaintScene;
use blitz_dom::{
    BaseDocument, DocumentConfig, FontContext, LocalName, NodeId, QualName, UserAgentStylesheet,
    ns,
};
use blitz_traits::shell::{ColorScheme, Viewport};

const CSS: &str = "
    html, body { margin: 0; height: 100%; }
    body { font-family: 'Noto Sans', sans-serif; font-size: 13px; }
    .rows { height: 100%; overflow-y: scroll; display: flex; flex-direction: column; }
    .row { display: flex; gap: 8px; padding: 4px 12px; border-bottom: 1px solid #ddd; }
    .row.selected { background: #dde7ff; }
    .title { flex: 1; }
    .chip { padding: 0 6px; border-radius: 4px; background: #eee; font-size: 11px; }
";

fn name(local: &str) -> QualName {
    QualName::new(None, ns!(html), LocalName::from(local))
}

fn element(document: &mut BaseDocument, tag: &str, class: &str) -> NodeId {
    let mut mutator = document.mutate();
    let node = mutator.create_element(name(tag), Vec::new());
    mutator.set_attribute(node, QualName::new(None, ns!(), LocalName::from("class")), class);
    node
}

fn main() {
    let mut arguments = std::env::args().skip(1);
    let rows: usize = (arguments.next()).map_or(1000, |rows| rows.parse().expect("a number"));
    let mut fonts = FontContext::new();
    let mut registered = 0;
    for path in arguments {
        let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
        let blob = peniko::Blob::new(Arc::new(bytes));
        registered += fonts.collection.register_fonts(blob, None).len();
    }

    let started = Instant::now();
    let mut document = BaseDocument::new(DocumentConfig {
        viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
        font_ctx: Some(fonts),
        ..DocumentConfig::default()
    });
    document.add_user_agent_stylesheet(UserAgentStylesheet::parse(CSS));
    let html = element(&mut document, "html", "");
    let body = element(&mut document, "body", "");
    let list = element(&mut document, "div", "rows");
    let top = document.root_node().id;
    document.mutate().append_children(top, &[html]);
    document.mutate().append_children(html, &[body]);
    document.mutate().append_children(body, &[list]);
    let mut made = Vec::new();
    for row in 0..rows {
        let line = element(&mut document, "div", "row");
        let title = element(&mut document, "div", "title");
        let chip = element(&mut document, "span", "chip");
        let mut mutator = document.mutate();
        let text = mutator.create_text_node(&format!("{row} 番目のタスク: task number {row}"));
        let label = mutator.create_text_node("doing");
        mutator.append_children(title, &[text]);
        mutator.append_children(chip, &[label]);
        mutator.append_children(line, &[title, chip]);
        mutator.append_children(list, &[line]);
        made.push(line);
    }
    let built = started.elapsed();

    let started = Instant::now();
    document.resolve(0.0);
    let first_layout = started.elapsed();

    let mut scene = anyrender::Scene::new();
    let started = Instant::now();
    blitz_paint::paint_scene(&mut scene, &mut document, 1.0, 800, 600, 0, 0);
    let first_scene = started.elapsed();
    let commands = scene.commands.len();
    // What the commands of one frame come to as data: glyphs are a number
    // and a place each, paths their elements, and a font is named, not
    // carried.
    let (mut glyphs, mut runs, mut path_elements) = (0, 0, 0);
    for command in &scene.commands {
        match command {
            anyrender::recording::RenderCommand::GlyphRun(run) => {
                runs += 1;
                glyphs += run.glyphs.len();
            }
            anyrender::recording::RenderCommand::Fill(fill) => {
                path_elements += fill.shape.elements().len();
            }
            anyrender::recording::RenderCommand::Stroke(stroke) => {
                path_elements += stroke.shape.elements().len();
            }
            _ => {}
        }
    }
    let bytes = glyphs * 12 + path_elements * 28 + commands * 64;
    println!("frame: {commands} commands, {runs} glyph runs, {glyphs} glyphs, {path_elements} path elements, about {bytes} bytes");

    // One row changes, as a step of a selection does, a hundred times.
    let class = QualName::new(None, ns!(), LocalName::from("class"));
    let (mut layouts, mut scenes) = (Vec::new(), Vec::new());
    for step in 0..100 {
        let (before, now) = (made[step % rows], made[(step + 1) % rows]);
        document.mutate().set_attribute(before, class.clone(), "row");
        document.mutate().set_attribute(now, class.clone(), "row selected");
        let started = Instant::now();
        document.resolve(0.0);
        layouts.push(started.elapsed());
        scene.reset();
        let started = Instant::now();
        blitz_paint::paint_scene(&mut scene, &mut document, 1.0, 800, 600, 0, 0);
        scenes.push(started.elapsed());
    }
    layouts.sort();
    scenes.sort();

    let height = (document.get_node(made[0])).map_or(0., |node| node.final_layout().size.height);
    println!(
        "rows={rows} fonts={registered} row_height={height} build={built:?} first_layout={first_layout:?} \
         first_scene={first_scene:?} commands={commands} step_layout_median={:?} step_scene_median={:?}",
        layouts[layouts.len() / 2],
        scenes[scenes.len() / 2],
    );
}
