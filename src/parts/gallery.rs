//! Every part in every kind it has, on one page: what the parts are
//! looked at on (the `parts` preview, `tests/parts.rs`'s picture).

use horizon_ui::{El, el};

use super::{Tone, chip};

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

/// A button as one of the views draws it today, before there is a part:
/// the view's own rules, written out on the element.
fn drawn(style: &str, label: &str) -> El {
    el("span").attr("style", style).text(label)
}

/// The buttons the views each draw their own way, side by side.
fn buttons_today() -> El {
    let agent = "flex: none; padding: 0 8px; border-radius: 6px; font-size: 12px; \
                 line-height: 20px; color: var(--text); cursor: pointer;";
    let agent_large = "flex: none; padding: 0 8px; border-radius: 6px; font-size: 14px; \
                       line-height: var(--u28); color: var(--text); cursor: pointer;";
    let thread = "padding: 0 var(--u12); border-radius: 4px; line-height: var(--u28); \
                  font-size: var(--meta-size); cursor: pointer;";
    let wiki = "padding: 0 var(--u12); border-radius: 4px; line-height: var(--u24); \
                font-size: var(--meta-size); border: 1px solid var(--border); cursor: pointer;";
    let primary = " background: var(--accent); color: var(--on-accent);";
    part(
        "button (as each view draws it today)",
        vec![
            (
                "agent: Stop, then Approve and Deny",
                vec![
                    drawn(agent, "Stop"),
                    drawn(&[agent_large, primary].concat(), "Approve"),
                    drawn(agent_large, "Deny"),
                ],
            ),
            (
                "board thread: the primary action and the others",
                vec![
                    drawn(&[thread, primary].concat(), "Send"),
                    drawn(thread, "Close task"),
                ],
            ),
            ("wiki", vec![drawn(wiki, "Send"), drawn(wiki, "Cancel")]),
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
        .child(buttons_today())
}
