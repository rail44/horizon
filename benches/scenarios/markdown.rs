//! The Markdown view over files of several lengths.

use std::rc::Rc;

use horizon::markdown::view;
use horizon::wake::Wake;
use horizon_theme_css::{Theme, themed};

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
    let directory = std::env::temp_dir().join(format!("horizon-scenarios-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a directory");
    let path = directory.join(format!("{sections}.md"));
    std::fs::write(&path, document(sections)).expect("the document written");
    let shown = path.clone();
    let mut stage = Stage::new(800, 800, horizon_theme_css::CSS, || {
        let (root, _) = view(Some(shown), Rc::new(|_: &str| {}), Wake::default());
        themed(Theme::built_in, root.focus())
    });
    stage.until_text("the document", "0 番目の節");
    for _ in 0..100 {
        stage.frame(|app| wheel(app, (400., 400.), -120.));
    }
    let _ = std::fs::remove_dir_all(&directory);
    stage.frames
}
