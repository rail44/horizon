//! `[layouts.<name>]`: trees the workspace can be put in, and
//! `startup_layout`, the one it is put in when Horizon starts
//! (`docs/workspace-layout-design.md`).
//!
//! A layout is kept as the file writes it and read into a [`Layout`] when
//! it is used: one that is not well formed is refused then, with what is
//! wrong with it, and leaves the rest of the file as it is.
//!
//! A node is a table with one of:
//!
//! - `view = "<name>"` and, if the view takes them, `arguments = [..]` --
//!   a view as `horizon new <name> <arguments>` makes it;
//! - `workspace = true` -- where the workspace as it is goes; at most one
//!   in a layout;
//! - `split = "horizontal" | "vertical"` and `children = [..]`, two or
//!   more, each of which may have a `weight` (1 where it has none);
//! - `tabs = [..]`, one or more, each a tab's tree.

use std::fmt;

/// A layout as it is read: a tree of views, splits and tab groups, with at
/// most one place for the workspace as it is.
#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    View {
        name: String,
        arguments: Vec<String>,
    },
    Workspace,
    Split {
        axis: LayoutAxis,
        /// Each child with its weight.
        children: Vec<(f32, Layout)>,
    },
    Tabs(Vec<Layout>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutAxis {
    Horizontal,
    Vertical,
}

/// What is wrong with a layout, and where in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutError(String);

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LayoutError {}

impl Layout {
    /// The layout `value` writes, as `[layouts.<name>]` holds it.
    pub fn read(value: &toml::Value) -> Result<Self, LayoutError> {
        let layout = node(value, "the layout", false)?;
        if layout.slots() > 1 {
            return Err(LayoutError(
                "a layout has one `workspace = true` at most".to_string(),
            ));
        }
        Ok(layout)
    }

    /// How many places for the workspace this tree has.
    fn slots(&self) -> usize {
        match self {
            Self::View { .. } => 0,
            Self::Workspace => 1,
            Self::Split { children, .. } => children.iter().map(|(_, child)| child.slots()).sum(),
            Self::Tabs(tabs) => tabs.iter().map(Self::slots).sum(),
        }
    }
}

/// The node `value` is, at `at` (what the error says it is); `weighed`
/// where it is a split's child, which may have a weight.
fn node(value: &toml::Value, at: &str, weighed: bool) -> Result<Layout, LayoutError> {
    let wrong = |what: String| LayoutError(format!("{at}: {what}"));
    let Some(table) = value.as_table() else {
        return Err(wrong("a table".to_string()));
    };
    let allowed = [
        "view",
        "arguments",
        "workspace",
        "split",
        "children",
        "tabs",
        "weight",
    ];
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(wrong(format!("no key `{key}`")));
    }
    if table.contains_key("weight") && !weighed {
        return Err(wrong("`weight` is a split's child's only".to_string()));
    }
    let kinds: Vec<&str> = ["view", "workspace", "split", "tabs"]
        .into_iter()
        .filter(|kind| table.contains_key(*kind))
        .collect();
    if kinds.len() != 1 {
        return Err(wrong(
            "one of `view`, `workspace = true`, `split` or `tabs`".to_string(),
        ));
    }
    if table.contains_key("arguments") && kinds[0] != "view" {
        return Err(wrong("`arguments` is a view's only".to_string()));
    }
    if table.contains_key("children") && kinds[0] != "split" {
        return Err(wrong("`children` is a split's only".to_string()));
    }
    match kinds[0] {
        "view" => {
            let name = (table["view"].as_str())
                .filter(|name| !name.is_empty())
                .ok_or_else(|| wrong("`view` names a view".to_string()))?;
            let arguments = match table.get("arguments") {
                None => Vec::new(),
                Some(value) => (value.as_array())
                    .and_then(|values| {
                        values
                            .iter()
                            .map(|value| value.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| wrong("`arguments` is a list of strings".to_string()))?,
            };
            Ok(Layout::View {
                name: name.to_string(),
                arguments,
            })
        }
        "workspace" => match table["workspace"].as_bool() {
            Some(true) => Ok(Layout::Workspace),
            _ => Err(wrong("`workspace = true`".to_string())),
        },
        "split" => {
            let axis = match table["split"].as_str() {
                Some("horizontal") => LayoutAxis::Horizontal,
                Some("vertical") => LayoutAxis::Vertical,
                _ => {
                    return Err(wrong(
                        "`split` is \"horizontal\" or \"vertical\"".to_string(),
                    ));
                }
            };
            let children = (table.get("children"))
                .and_then(toml::Value::as_array)
                .filter(|children| children.len() >= 2)
                .ok_or_else(|| wrong("a split has two `children` or more".to_string()))?;
            let children = (children.iter().enumerate())
                .map(|(index, child)| {
                    let at = format!("{at}'s child {}", index + 1);
                    let weight = match child.get("weight") {
                        None => 1.0,
                        Some(weight) => (weight.as_float())
                            .or_else(|| weight.as_integer().map(|weight| weight as f64))
                            .map(|weight| weight as f32)
                            .filter(|weight| weight.is_finite() && *weight > 0.0)
                            .ok_or_else(|| {
                                LayoutError(format!("{at}: `weight` is a number above 0"))
                            })?,
                    };
                    Ok((weight, node(child, &at, true)?))
                })
                .collect::<Result<Vec<_>, LayoutError>>()?;
            Ok(Layout::Split { axis, children })
        }
        _ => {
            let tabs = (table["tabs"].as_array())
                .filter(|tabs| !tabs.is_empty())
                .ok_or_else(|| wrong("`tabs` is a list of one tab or more".to_string()))?;
            let tabs = (tabs.iter().enumerate())
                .map(|(index, tab)| node(tab, &format!("{at}'s tab {}", index + 1), false))
                .collect::<Result<Vec<_>, LayoutError>>()?;
            Ok(Layout::Tabs(tabs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Result<Layout, LayoutError> {
        let table: toml::Table = toml::from_str(text).expect("toml");
        Layout::read(&toml::Value::Table(table))
    }

    #[test]
    fn a_list_on_the_left_of_the_workspace_is_read() {
        let layout = read(
            r#"
            split = "horizontal"
            children = [
              { weight = 1, view = "sessions" },
              { weight = 3, workspace = true },
            ]
            "#,
        )
        .expect("a layout");
        assert_eq!(
            layout,
            Layout::Split {
                axis: LayoutAxis::Horizontal,
                children: vec![
                    (
                        1.0,
                        Layout::View {
                            name: "sessions".to_string(),
                            arguments: Vec::new(),
                        }
                    ),
                    (3.0, Layout::Workspace),
                ],
            }
        );
    }

    #[test]
    fn tabs_hold_trees_and_views_take_arguments() {
        let layout = read(
            r#"
            tabs = [
              { view = "wiki", arguments = ["index"] },
              { split = "vertical", children = [{ view = "board-list" }, { workspace = true }] },
            ]
            "#,
        )
        .expect("a layout");
        let Layout::Tabs(tabs) = layout else {
            panic!("tabs");
        };
        assert_eq!(
            tabs[0],
            Layout::View {
                name: "wiki".to_string(),
                arguments: vec!["index".to_string()],
            }
        );
        assert!(matches!(&tabs[1], Layout::Split { children, .. } if children[0].0 == 1.0));
    }

    #[test]
    fn what_is_not_well_formed_is_refused_with_where() {
        for (text, said) in [
            (r#"view = """#, "the layout: `view` names a view"),
            (r#"workspace = false"#, "the layout: `workspace = true`"),
            (
                r#"view = "a"
                split = "horizontal""#,
                "the layout: one of",
            ),
            (
                r#"split = "across"
                children = [{ view = "a" }, { view = "b" }]"#,
                "\"horizontal\" or \"vertical\"",
            ),
            (
                r#"split = "horizontal"
                children = [{ view = "a" }]"#,
                "two `children` or more",
            ),
            (
                r#"split = "horizontal"
                children = [{ view = "a" }, { view = "b", weight = 0 }]"#,
                "child 2: `weight`",
            ),
            (r#"tabs = []"#, "one tab or more"),
            (
                r#"view = "a"
                weight = 2"#,
                "a split's child's only",
            ),
            (
                r#"view = "a"
                colour = "red""#,
                "no key `colour`",
            ),
            (
                r#"split = "horizontal"
                children = [{ workspace = true }, { workspace = true }]"#,
                "one `workspace = true` at most",
            ),
        ] {
            let error = read(text).expect_err(text).to_string();
            assert!(error.contains(said), "{text}: {error}");
        }
    }

    #[test]
    fn the_file_names_its_layouts_and_the_one_at_startup() {
        let config = crate::parse(
            r#"
            startup_layout = "sidebar"

            [layouts.sidebar]
            split = "horizontal"
            children = [{ view = "sessions" }, { workspace = true }]

            [layouts.broken]
            view = 3
            "#,
        )
        .expect("a file with a broken layout is read");
        assert_eq!(config.startup_layout.as_deref(), Some("sidebar"));
        assert!(config.layout("sidebar").is_ok());
        let broken = config.layout("broken").expect_err("broken");
        assert!(broken.starts_with("the layout `broken`: "), "{broken}");
        assert_eq!(config.layout("none").expect_err("none"), "no layout `none`");
    }
}
