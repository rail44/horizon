//! Every part in every kind it has, on one page: what the parts are
//! looked at on (the `parts` preview, `tests/parts.rs`'s picture).

use horizon_ui::{Component, El, el};

use super::{Button, ButtonKind, ButtonSize, Chip, Field, Prompt, Tone};

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
    el("div")
        .class(styles::PART)
        .child(el("div").class(styles::NAME).text(name))
        .children(lines.into_iter().map(|(said, shown)| {
            el("div")
                .class(styles::LINE)
                .children(shown)
                .child(el("span").class(styles::SAID).text(said))
        }))
}

fn buttons() -> El {
    use ButtonKind::Primary;
    use ButtonSize::Small;
    part(
        "button",
        vec![
            (
                "the one thing to do next, and the others beside it",
                vec![
                    Button {
                        label: "Reply",
                        kind: Primary,
                        ..Default::default()
                    }
                    .view(),
                    Button {
                        label: "Close",
                        ..Default::default()
                    }
                    .view(),
                    Button {
                        label: "Session",
                        ..Default::default()
                    }
                    .view(),
                ],
            ),
            (
                "small, in a line of text",
                vec![
                    Button {
                        label: "Stop",
                        size: Small,
                        ..Default::default()
                    }
                    .view(),
                    Button {
                        label: "Approve",
                        kind: Primary,
                        size: Small,
                        ..Default::default()
                    }
                    .view(),
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
                boxed(
                    Field {
                        input: el("input").attr("type", "text"),
                        prompt: Some(Prompt {
                            text: "Add a task...".into(),
                            shown: true.into(),
                        }),
                    }
                    .view(),
                ),
                boxed(
                    Field {
                        input: typed,
                        prompt: None,
                    }
                    .view(),
                ),
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
                    tones
                        .iter()
                        .map(|(tone, name)| {
                            Chip {
                                tone: *tone,
                                text: name,
                            }
                            .view()
                        })
                        .collect(),
                ),
                (
                    "as views say things",
                    vec![
                        Chip {
                            tone: Tone::Accent,
                            text: "doing",
                        }
                        .view(),
                        Chip {
                            tone: Tone::Info,
                            text: "review",
                        }
                        .view(),
                        Chip {
                            tone: Tone::Accent,
                            text: "3 unread",
                        }
                        .view(),
                        Chip {
                            tone: Tone::Danger,
                            text: "Failed",
                        }
                        .view(),
                        Chip {
                            tone: Tone::Muted,
                            text: "Stopped",
                        }
                        .view(),
                        Chip {
                            tone: Tone::Muted,
                            text: "Enter",
                        }
                        .view(),
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
