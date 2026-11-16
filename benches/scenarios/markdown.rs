//! Markdown of several lengths, drawn as the views draw it
//! (`horizon_components::markdown`) and scrolled. The Markdown view
//! itself is an extension's (`plugins/markdown`), and is not timed here.

use std::rc::Rc;

use horizon_components::markdown::{Headings, render};
use horizon_theme_css::{Theme, themed};
use horizon_ui::el;

use crate::{Frame, Scenario, Stage, wheel};

pub fn scenarios() -> Vec<Scenario> {
    vec![Scenario {
        name: "markdown/scroll",
        unit: "sections",
        sizes: &[10, 100, 1000],
        run: scroll,
    }]
}

/// A document of `sections` sections: a heading, a paragraph, a list and
/// a block of code each.
fn document(sections: usize) -> String {
    let mut text = String::from("# 長い文書\n\n");
    for section in 0..sections {
        text += &format!(
            "## {section} 番目の節\n\n\
             この節は `horizon-ui` の `Page` が読むものを説明します。**太字**と[リンク](https://example.com/{section})を含みます。\n\n\
             - 一つ目の項目\n- 二つ目の項目\n- 三つ目の項目\n\n\
             ```rust\nfn section_{section}() -> usize {{\n    {section}\n}}\n```\n\n"
        );
    }
    text
}

fn scroll(sections: usize) -> Vec<Frame> {
    let text = document(sections);
    let mut stage = Stage::new(800, 800, horizon_theme_css::CSS, || {
        let root = el("div")
            .attr("style", "height: 100%; overflow-y: scroll;")
            .child(render(&text, Headings::Scaled, Rc::new(|_: &str| {})));
        themed(Theme::built_in, root)
    });
    stage.until_text("the document", "0 番目の節");
    for _ in 0..100 {
        stage.frame(|app| wheel(app, (400., 400.), -120.));
    }
    stage.frames
}
