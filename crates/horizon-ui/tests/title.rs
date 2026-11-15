//! The page's title: the name the shell gives the window when it creates
//! it, taken from the document's `head`.

use horizon_ui::{App, el};

#[test]
fn the_page_s_title_is_the_one_the_shell_names_the_window_by() {
    let app = App::new(400, 300, "Horizon", "", || el("div"));
    app.resolve();
    let document = app.document();
    let document = document.borrow();
    let title = document.find_title_node().map(|node| node.text_content());
    assert_eq!(title.as_deref(), Some("Horizon"));
}
