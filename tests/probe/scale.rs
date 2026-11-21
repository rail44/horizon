use super::*;

#[test]
#[ignore]
fn probe_focus_line_on_boundary_at_fractional_scale() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
    use horizon_workspace::commands::CommandId;
    let scale: f32 = std::env::var("PROBE_SCALE").map_or(1.5, |s| s.parse().unwrap());
    let mut summary: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for down in [false, true] {
        for focus in [0usize, 1] {
            for extra in 0..24u32 {
                let (width, height) = if down {
                    (1200u32, 780 + extra)
                } else {
                    (1180 + extra, 800u32)
                };
                let opened = open();
                (opened.app.document().borrow_mut())
                    .set_viewport(Viewport::new(width, height, scale, ColorScheme::Light));
                opened.app.resolve();
                opened.shell.execute(if down {
                    CommandId::SplitDown
                } else {
                    CommandId::SplitRight
                });
                opened.app.resolve();
                if focus == 0 {
                    opened.chord();
                    opened.character(if down { "k" } else { "h" });
                    opened.named(NamedKey::Enter);
                }
                assert!(!opened.mode());
                for _ in 0..3 {
                    opened.app.run_effects();
                    opened.app.resolve();
                }
                opened.app.resolve();
                let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
                let mut pixels = Vec::new();
                let document = opened.app.document();
                renderer.render_to_vec(
                    |scene| {
                        let mut document = document.borrow_mut();
                        blitz_paint::paint_scene(
                            scene,
                            &mut document,
                            scale as f64,
                            width,
                            height,
                            0,
                            0,
                        )
                    },
                    &mut pixels,
                );
                let at = |x: i64, y: i64| {
                    let pixel = (y as usize * width as usize + x as usize) * 4;
                    [pixels[pixel], pixels[pixel + 1], pixels[pixel + 2]]
                };
                let Some(divider) = opened.all("divider").first().copied() else {
                    summary
                        .entry(format!("down={down} focus={focus} NO DIVIDER"))
                        .or_default()
                        .push(format!("{width}x{height}"));
                    continue;
                };
                let (x, y, wide, tall) = opened.bounds(divider);
                let line: Vec<(i64, [u8; 3])> = if down {
                    let column = ((x + wide / 2.) * scale) as i64;
                    let (from, to) = ((y * scale) as i64 - 3, ((y + tall) * scale) as i64 + 4);
                    let background = at(column, from);
                    (from..to)
                        .map(|r| (r, at(column, r)))
                        .filter(|(_, p)| *p != background)
                        .collect()
                } else {
                    let row = ((y + tall / 2.) * scale) as i64;
                    let (from, to) = ((x * scale) as i64 - 3, ((x + wide) * scale) as i64 + 4);
                    let background = at(from, row);
                    (from..to)
                        .map(|c| (c, at(c, row)))
                        .filter(|(_, p)| *p != background)
                        .collect()
                };
                if let Ok(dump) = std::env::var("PROBE_DUMP")
                    && extra == 0
                {
                    let mut picture = format!("P6\n{width} {height}\n255\n").into_bytes();
                    for [red, green, blue, _] in pixels.as_chunks::<4>().0 {
                        picture.extend_from_slice(&[*red, *green, *blue]);
                    }
                    std::fs::write(format!("{dump}-{down}-{focus}.ppm"), picture).unwrap();
                    eprintln!(
                        "dump down={down} focus={focus}: focused={} part={} divider={:?} panes={:?}",
                        opened.all("focused").len(),
                        opened.all("part").len(),
                        (x * scale, y * scale, wide * scale, tall * scale),
                        opened
                            .all("pane")
                            .iter()
                            .map(|p| {
                                let b = opened.bounds(*p);
                                (b.0 * scale, b.1 * scale, b.2 * scale, b.3 * scale)
                            })
                            .collect::<Vec<_>>()
                    );
                }
                let start = if down { y * scale } else { x * scale };
                let pattern: Vec<String> = line
                    .iter()
                    .map(|(_, p)| format!("{:02x}{:02x}{:02x}", p[0], p[1], p[2]))
                    .collect();
                let key = format!("down={down} focus={focus} {pattern:?}");
                summary
                    .entry(key)
                    .or_default()
                    .push(format!("{width}x{height} divider@{start:.3}dp"));
            }
        }
    }
    for (key, sizes) in summary {
        eprintln!("{key}\n    {} cases: {}", sizes.len(), sizes.join(", "));
    }
}

/// Every side of every pane, focused in turn, in several arrangements:
/// the pixels across the side at its middle, as a pattern.
#[test]
#[ignore]
fn probe_every_side_of_every_focused_pane() {
    use anyrender::ImageRenderer as _;
    use horizon_ui::blitz_traits::shell::{ColorScheme, Viewport};
    use horizon_workspace::commands::CommandId;
    let scale: f32 = std::env::var("PROBE_SCALE").map_or(1.5, |s| s.parse().unwrap());
    let arrangements: Vec<(&str, Vec<CommandId>)> = vec![
        ("3 across", vec![CommandId::SplitRight, CommandId::SplitRight]),
        ("3 down", vec![CommandId::SplitDown, CommandId::SplitDown]),
        ("right then down", vec![CommandId::SplitRight, CommandId::SplitDown]),
        ("down then right", vec![CommandId::SplitDown, CommandId::SplitRight]),
        (
            "2x2",
            vec![CommandId::SplitRight, CommandId::SplitDown, CommandId::SplitRight],
        ),
    ];
    let mut summary: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut bad = 0;
    let mut total = 0;
    for (name, commands) in &arrangements {
        let sizes: Vec<(u32, u32)> = if std::env::var_os("PROBE_SWEEP").is_some() {
            (0..60u32).map(|i| (1180 + i, if std::env::var_os("PROBE_RESIZE").is_some() { 800 } else { 760 + (i * 7) % 61 })).collect()
        } else {
            vec![(1200u32, 800u32), (1201, 801), (1213, 797), (1366, 768), (997, 703)]
        };
        for (width, height) in sizes {
            let opened = open();
            (opened.app.document().borrow_mut())
                .set_viewport(Viewport::new(width, height, scale, ColorScheme::Light));
            opened.app.resolve();
            for command in commands {
                opened.shell.execute(*command);
                for _ in 0..3 {
                    opened.app.run_effects();
                    opened.app.resolve();
                }
            }
            let mut variant = String::new();
            if std::env::var_os("PROBE_RESIZE").is_some() {
                let i = (width - 1180) as usize;
                let (across, down) = (i % 8, i / 8);
                opened.chord();
                for _ in 0..across {
                    opened.character("<");
                }
                for _ in 0..down {
                    opened.character("-");
                }
                opened.named(NamedKey::Escape);
                for _ in 0..3 {
                    opened.app.run_effects();
                    opened.app.resolve();
                }
                variant = format!(" <x{across} -x{down}");
            }
            let panes = opened.panes();
            for (index, pane) in panes.iter().enumerate() {
                opened.shell.model.update(|model| _ = model.activate_pane(*pane));
                for _ in 0..3 {
                    opened.app.run_effects();
                    opened.app.resolve();
                }
                let elements = opened.all("pane");
                let focused: Vec<_> = opened.all("focused");
                let element = elements[index];
                assert!(focused.contains(&element), "{name}: pane {index} focused");
                let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
                let mut pixels = Vec::new();
                let document = opened.app.document();
                renderer.render_to_vec(
                    |scene| {
                        let mut document = document.borrow_mut();
                        blitz_paint::paint_scene(
                            scene,
                            &mut document,
                            scale as f64,
                            width,
                            height,
                            0,
                            0,
                        )
                    },
                    &mut pixels,
                );
                let at = |x: i64, y: i64| {
                    let pixel = (y as usize * width as usize + x as usize) * 4;
                    format!(
                        "{:02x}{:02x}{:02x}",
                        pixels[pixel],
                        pixels[pixel + 1],
                        pixels[pixel + 2]
                    )
                };
                let (x, y, wide, tall) = opened.bounds(element);
                let (x0, y0) = ((x * scale) as i64, (y * scale) as i64);
                let (x1, y1) = (((x + wide) * scale) as i64, ((y + tall) * scale) as i64);
                let (mx, my) = (x0 + (x1 - x0) / 3, y0 + (y1 - y0) / 3);
                let sides = [
                    ("left", (x0 - 4..x0 + 4).map(|c| at(c, my)).collect::<Vec<_>>()),
                    ("right", (x1 - 4..x1 + 4).map(|c| at(c, my)).collect()),
                    ("top", (y0 - 4..y0 + 4).map(|r| at(mx, r)).collect()),
                    ("bottom", (y1 - 4..y1 + 4).map(|r| at(mx, r)).collect()),
                ];
                for (side, run) in sides {
                    total += 1;
                    let accents = run.iter().filter(|p| *p == "84dcc6").count();
                    // Collapse runs of one color.
                    let mut pattern: Vec<&str> = Vec::new();
                    for p in &run {
                        if pattern.last() != Some(&p.as_str()) {
                            pattern.push(p);
                        }
                    }
                    let ok = accents == 1 && !run.iter().any(|p| p == "30323a");
                    if !ok {
                        let divider = opened
                            .all("divider")
                            .iter()
                            .map(|d| {
                                let b = opened.bounds(*d);
                                (b.0 * scale, b.1 * scale, b.2 * scale, b.3 * scale)
                            })
                            .collect::<Vec<_>>();
                        {
                            let document = opened.app.document();
                            let document = document.borrow();
                            let chain = |id: NodeId| {
                                let mut out = Vec::new();
                                let mut at = Some(id);
                                while let Some(n) = at {
                                    let node = document.get_node(n).unwrap();
                                    let f = node.final_layout();
                                    let u = node.unrounded_layout();
                                    out.push(format!(
                                        "[{} {:?} f=({},{} {}x{} bt{}) u=({},{} {}x{} bt{})]",
                                        n,
                                        node.element_data()
                                            .and_then(|e| e.attr(horizon_ui::blitz_dom::local_name!("class")))
                                            .unwrap_or(""),
                                        f.location.x * scale, f.location.y * scale,
                                        f.size.width * scale, f.size.height * scale,
                                        f.border.top * scale,
                                        u.location.x * scale, u.location.y * scale,
                                        u.size.width * scale, u.size.height * scale,
                                        u.border.top * scale,
                                    ));
                                    at = node.layout_parent.get().or(node.parent);
                                    if out.len() > 9 {
                                        break;
                                    }
                                }
                                out.join("\n        ")
                            };
                            for d in opened.all("divider") {
                                eprintln!("  DIVIDER chain:\n        {}", chain(d));
                            }
                            eprintln!("  PANE chain:\n        {}", chain(element));
                        }
                        eprintln!(
                            "RAW {name} {width}x{height}{variant} pane{index} {side}: {run:?} pane=({:.3},{:.3},{:.3},{:.3}) dividers={divider:?}",
                            x * scale,
                            y * scale,
                            wide * scale,
                            tall * scale
                        );
                    }
                    if !ok {
                        bad += 1;
                    }
                    summary
                        .entry(format!("{} {side} {pattern:?}", if ok { "ok " } else { "BAD" }))
                        .or_default()
                        .push(format!("{name} {width}x{height}{variant} pane{index}"));
                }
            }
        }
    }
    for (key, cases) in &summary {
        let shown: Vec<_> = cases.iter().take(6).cloned().collect();
        eprintln!("{key}\n    {} cases: {}", cases.len(), shown.join(", "));
    }
    eprintln!("bad {bad} of {total}");
}
