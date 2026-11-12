//! The theme as CSS variables: what the built-in scheme and type scale
//! come out as, and a change of them reaching a view through the one
//! attribute.

use horizon_theme_css::{CSS, Theme, themed};
use horizon_ui::reactive_graph::signal::RwSignal;
use horizon_ui::reactive_graph::traits::{Get, Set};
use horizon_ui::{App, el};

#[test]
fn the_built_in_theme_is_the_default_scheme_and_the_scale_at_thirteen() {
    let variables = Theme::built_in().variables();
    for expected in [
        // `horizon-theme`'s defaults.
        "--background: #16181d;",
        "--text: #e9ecf2;",
        // `src/ui/spec.rs` at a base of 13: body 13/24, meta 11/17.
        "--body-size: 13px; --body-line: 24px;",
        "--meta-size: 11px; --meta-line: 17px;",
        "--u8: 7px;",
        "--u24: 22px;",
        "--font-family: \"DejaVu Sans Mono\";",
    ] {
        assert!(variables.contains(expected), "{expected} in {variables}");
    }
}

#[test]
fn a_change_of_theme_reaches_a_view_through_its_variables() {
    let look = RwSignal::new(Theme::built_in());
    let mut line = None;
    let css = [CSS, ".line { height: var(--body-line); }"].concat();
    let app = App::new(400, 300, &css, || {
        let element = el("div").class("line");
        line = Some(element);
        themed(move || look.get(), element)
    });
    let line = line.unwrap();
    let height = |app: &App| {
        app.resolve();
        let document = app.document();
        let document = document.borrow();
        document
            .get_node(line.0)
            .unwrap()
            .final_layout()
            .size
            .height
    };
    assert_eq!(height(&app), 24.);

    // The font size doubled: every scaled length follows.
    let mut larger = Theme::built_in();
    larger.font_size = 26.;
    look.set(larger);
    app.run_effects();
    assert_eq!(height(&app), 48.);
}
