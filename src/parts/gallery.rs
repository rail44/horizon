//! Every part in every kind it has, on one page: what the parts are
//! looked at on (the `parts` preview, `tests/parts.rs`'s picture).

use horizon_ui::{El, el};

use super::{ButtonKind, ButtonSize, Tone, button, chip, field, prompt};

mod styles {
    horizon_ui::css_module! {"
        .gallery { display: flex; flex-direction: column; gap: var(--u24); height: 100%;
                   box-sizing: border-box; padding: var(--u24); overflow-y: scroll;
                   background: var(--background); color: var(--text);
                   font-family: var(--font-family); font-size: var(--body-size);
                   line-height: var(--body-line); }
        .part { display: flex; flex-direction: column; gap: var(--u8); }
        .name { font-weight: 600; }
        .line { display: flex; align-items: flex-start; gap: var(--u8); }
        .said { font-size: var(--meta-size); line-height: var(--meta-line);
                color: var(--text-muted); }
    "}
}

/// One part of the gallery: its name, and lines of it as it is placed,
/// each with what the line is.
fn part(name: &str, lines: Vec<(&str, Vec<El>)>) -> El {
    let part = el("div")
        .class(styles::PART)
        .child(el("div").class(styles::NAME).text(name));
    for (said, shown) in lines {
        let line = el("div").class(styles::LINE);
        for one in shown {
            line.child(one);
        }
        part.child(line.child(el("span").class(styles::SAID).text(said)));
    }
    part
}

fn buttons() -> El {
    use ButtonKind::{Plain, Primary};
    use ButtonSize::{Regular, Small};
    part(
        "button",
        vec![
            (
                "the one thing to do next, and the others beside it",
                vec![
                    button(Primary, Regular, "Reply"),
                    button(Plain, Regular, "Close"),
                    button(Plain, Regular, "Session"),
                ],
            ),
            (
                "small, in a line of text",
                vec![
                    button(Plain, Small, "Stop"),
                    button(Primary, Small, "Approve"),
                ],
            ),
        ],
    )
}

/// A field's box, empty with its prompt and with something typed.
fn fields() -> El {
    let boxed = |field: El| {
        el("div")
            .attr(
                "style",
                "flex: none; width: 240px; padding: var(--u4) var(--u8); \
                 border: var(--line) solid var(--border); border-radius: var(--radius);",
            )
            .child(field)
    };
    let typed = el("input").attr("type", "text");
    typed.set_value("something typed");
    part(
        "field",
        vec![(
            "empty with its prompt, and typed in (the box around is whoever places it's)",
            vec![
                boxed(field(el("input").attr("type", "text")).child(prompt("Add a task..."))),
                boxed(field(typed)),
            ],
        )],
    )
}

/// Rendered Markdown, with each way its headings are drawn. (Not in a
/// plugin: the renderer is not built for one.)
#[cfg(not(target_family = "wasm"))]
fn markdown() -> El {
    use std::rc::Rc;

    use crate::markdown::{Headings, render};

    const SAMPLE: &str = "# A heading\n\nA paragraph with `code` in it, a [link](there), and \
        **bold**.\n\n## A second heading\n\n- one item\n- another\n\n```rust\nfn main() \
        {}\n```\n\n> Something quoted.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n";
    let shown = |headings| {
        el("div")
            .attr("style", "flex: 1; min-width: 0;")
            .child(render(SAMPLE, headings, Rc::new(|_| {})))
    };
    part(
        "markdown",
        vec![(
            "a page or a message, and a post among posts",
            vec![shown(Headings::Scaled), shown(Headings::AsText)],
        )],
    )
}

/// The gallery.
pub fn gallery() -> El {
    let tones = [
        (Tone::Muted, "muted"),
        (Tone::Accent, "accent"),
        (Tone::Success, "success"),
        (Tone::Info, "info"),
        (Tone::Warning, "warning"),
        (Tone::Danger, "danger"),
    ];
    let gallery = el("div")
        .class(styles::GALLERY)
        .child(part(
            "chip",
            vec![
                (
                    "every tone",
                    tones.iter().map(|(tone, name)| chip(*tone, name)).collect(),
                ),
                (
                    "as views say things",
                    vec![
                        chip(Tone::Accent, "doing"),
                        chip(Tone::Info, "review"),
                        chip(Tone::Accent, "3 unread"),
                        chip(Tone::Danger, "Failed"),
                        chip(Tone::Muted, "Stopped"),
                        chip(Tone::Muted, "Enter"),
                    ],
                ),
            ],
        ))
        .child(buttons())
        .child(fields());
    #[cfg(not(target_family = "wasm"))]
    gallery.child(markdown());
    gallery
}
