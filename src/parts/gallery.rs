//! Every part in every kind it has, on one page: what the parts are
//! looked at on (the `parts` preview, `tests/parts.rs`'s picture).

use horizon_ui::{El, el};

use super::{ButtonKind, ButtonSize, Tone, button, chip};

mod styles {
    horizon_ui::css_module! {"
        .gallery { display: flex; flex-direction: column; gap: var(--u24); height: 100%;
                   box-sizing: border-box; padding: var(--u24); overflow-y: scroll;
                   background: var(--background); color: var(--text);
                   font-family: var(--font-family); font-size: var(--body-size);
                   line-height: var(--body-line); }
        .part { display: flex; flex-direction: column; gap: var(--u8); }
        .name { font-weight: 600; }
        .line { display: flex; align-items: center; gap: var(--u8); }
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
    el("div")
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
}
