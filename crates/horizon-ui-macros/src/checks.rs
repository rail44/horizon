//! What a CSS module is held to, beyond being CSS: its values come from
//! the theme, and its rules are over its own elements.
//!
//! - A rule's selector names a class of the sheet. The classes are what
//!   the module gives names no other sheet has; a rule without one
//!   (`div { .. }`, `* { .. }`) is over every view in the document, which
//!   the views share. (A rule inside another is under that one's, and
//!   `:where(.a, .b)` or `:is(..)` names a class when each of its
//!   selectors does.)
//! - There is no `@layer`: the document's sheets are not layered, and the
//!   engine does not take one that is among them.
//! - A color is one of the theme's variables, not written out
//!   (`transparent` and `currentColor` are not the theme's to give),
//!   unless the sheet is let off that.
//! - A `font-size` or a `line-height` is not a number of pixels: the type
//!   scale's variables have those.
//! - A variable read with nothing to fall back on is the theme's or one
//!   the sheet sets itself. (One read with a fallback is how a part takes
//!   what whoever places it may set.)
//! - Nothing is `!important`.
//! - A property is one CSS has: a mistyped name is otherwise a declaration
//!   that does nothing, silently.

use std::collections::HashSet;
use std::convert::Infallible;

use lightningcss::declaration::DeclarationBlock;
use lightningcss::properties::Property;
use lightningcss::properties::custom::{CustomPropertyName, Variable};
use lightningcss::properties::font::{FontSize, LineHeight};
use lightningcss::rules::CssRule;
use lightningcss::selector::{Component, Selector};
use lightningcss::stylesheet::{PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;
use lightningcss::values::color::CssColor;
use lightningcss::values::length::{LengthPercentage, LengthValue};
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

/// Properties CSS has that Lightning CSS does not know by name: a name
/// that is neither one it knows nor one of these is taken for a mistyped
/// one.
const UNNAMED: &[&str] = &[
    "content",
    "object-fit",
    "outline-offset",
    "pointer-events",
    "scroll-behavior",
    "scrollbar-width",
    "text-wrap",
];

/// The theme's variables, as `horizon-theme-css` sets them: the file is
/// that crate's, and it checks that it sets these and no others.
const THEME: &str = include_str!("../../horizon-theme-css/variables.txt");

#[derive(Default)]
struct Checks {
    /// The sheet may write colors out.
    written_colors: bool,
    /// The property being looked into, as the sheet names it.
    property: String,
    set: HashSet<String>,
    read: Vec<String>,
    faults: Vec<String>,
}

impl Checks {
    fn color(&mut self, color: &CssColor) {
        if self.written_colors {
            return;
        }
        let given = match color {
            CssColor::CurrentColor => true,
            CssColor::RGBA(color) => color.alpha == 0,
            CssColor::LightDark(light, dark) => {
                self.color(light);
                self.color(dark);
                true
            }
            _ => false,
        };
        if !given {
            let written = (color.to_css_string(PrinterOptions::default())).unwrap_or_default();
            self.faults.push(format!(
                "`{}` has the color {written} written out: a color is one of the theme's \
                 variables",
                self.property
            ));
        }
    }
}

fn in_pixels(length: &LengthPercentage) -> bool {
    matches!(length, LengthPercentage::Dimension(LengthValue::Px(_)))
}

impl<'i> Visitor<'i> for Checks {
    type Error = Infallible;

    fn visit_types(&self) -> VisitTypes {
        visit_types!(PROPERTIES | COLORS | VARIABLES)
    }

    fn visit_declaration_block(
        &mut self,
        declarations: &mut DeclarationBlock<'i>,
    ) -> Result<(), Self::Error> {
        for property in &declarations.important_declarations {
            self.faults.push(format!(
                "`{}` is `!important`: what a rule is over is said by its selector",
                property.property_id().name()
            ));
        }
        declarations.visit_children(self)
    }

    fn visit_property(&mut self, property: &mut Property<'i>) -> Result<(), Self::Error> {
        self.property = property.property_id().name().to_string();
        match property {
            Property::Custom(custom) => match &custom.name {
                CustomPropertyName::Custom(name) => {
                    self.set.insert(name.0.to_string());
                }
                CustomPropertyName::Unknown(name) if !UNNAMED.contains(&&*name.0) => {
                    self.faults
                        .push(format!("`{}` is not a property CSS has", name.0));
                }
                CustomPropertyName::Unknown(_) => {}
            },
            Property::FontSize(FontSize::Length(length)) if in_pixels(length) => {
                self.faults.push(
                    "`font-size` is a number of pixels: the type scale has the sizes \
                     (`var(--body-size)`)"
                        .to_string(),
                );
            }
            Property::LineHeight(LineHeight::Length(length)) if in_pixels(length) => {
                self.faults.push(
                    "`line-height` is a number of pixels: the type scale has the line heights \
                     (`var(--body-line)`)"
                        .to_string(),
                );
            }
            _ => {}
        }
        property.visit_children(self)
    }

    fn visit_color(&mut self, color: &mut CssColor) -> Result<(), Self::Error> {
        self.color(color);
        Ok(())
    }

    fn visit_variable(&mut self, variable: &mut Variable<'i>) -> Result<(), Self::Error> {
        if variable.fallback.is_none() {
            self.read.push(variable.name.ident.0.to_string());
        }
        variable.visit_children(self)
    }
}

/// Whether `selector` names a class: by itself, or in a `:where` or an
/// `:is` every selector of which does.
fn classed(selector: &Selector) -> bool {
    selector
        .iter_raw_match_order()
        .any(|component| match component {
            Component::Class(_) => true,
            Component::Where(selectors) | Component::Is(selectors) => {
                !selectors.is_empty() && selectors.iter().all(classed)
            }
            _ => false,
        })
}

/// What among `rules`, the sheet's own or a conditional rule's, is over
/// more than the sheet's elements.
fn unconfined(rules: &[CssRule], faults: &mut Vec<String>) {
    for rule in rules {
        match rule {
            CssRule::Style(style) => {
                for selector in &style.selectors.0 {
                    if !classed(selector) {
                        let written =
                            (selector.to_css_string(PrinterOptions::default())).unwrap_or_default();
                        faults.push(format!(
                            "the selector `{written}` names no class of this sheet: its rule \
                             would be over every view in the document"
                        ));
                    }
                }
            }
            CssRule::Media(media) => unconfined(&media.rules.0, faults),
            CssRule::Supports(supports) => unconfined(&supports.rules.0, faults),
            CssRule::Container(container) => unconfined(&container.rules.0, faults),
            CssRule::LayerBlock(_) | CssRule::LayerStatement(_) => faults.push(
                "`@layer`: the document's sheets are not layered, and the engine does not take \
                 one that is among them"
                    .to_string(),
            ),
            _ => {}
        }
    }
}

/// What in `sheet` is not as a CSS module is to be written, each said in
/// a line.
pub(crate) fn faults(sheet: &mut StyleSheet, written_colors: bool) -> Vec<String> {
    let mut checks = Checks {
        written_colors,
        ..Checks::default()
    };
    unconfined(&sheet.rules.0, &mut checks.faults);
    let Ok(()) = sheet.visit(&mut checks);
    let theme: HashSet<&str> = THEME.lines().map(str::trim).collect();
    let mut faults = checks.faults;
    let mut unknown: Vec<&String> = (checks.read.iter())
        .filter(|name| !theme.contains(name.as_str()) && !checks.set.contains(*name))
        .collect();
    unknown.sort();
    unknown.dedup();
    for name in unknown {
        faults.push(format!(
            "`var({name})` is not a variable of the theme's or one this sheet sets (a variable \
             whoever places this may set is read with a fallback: `var({name}, …)`)"
        ));
    }
    faults
}

#[cfg(test)]
mod tests {
    use lightningcss::stylesheet::ParserOptions;

    use super::*;

    fn faults_of(css: &str) -> Vec<String> {
        let mut sheet = StyleSheet::parse(css, ParserOptions::default()).expect("CSS");
        faults(&mut sheet, false)
    }

    #[test]
    fn a_rule_is_over_a_class_of_the_sheet() {
        // A class anywhere in the selector, and whatever is inside a rule
        // that has one.
        let confined = "
            .list { display: flex; > :first-child { margin: 0; } h1 { margin: 0; } }
            .list .row:hover, div.row { display: flex; }
            :where(.row) { flex: none; }
            :is(.row, .list) > p { margin: 0; }
            @media (min-width: 100px) { .list { display: block; } }
            @keyframes turn { to { opacity: 0; } }
        ";
        assert_eq!(faults_of(confined), Vec::<String>::new());

        for (css, selector) in [
            ("div { display: flex; }", "div"),
            ("* { margin: 0; }", "*"),
            (".row, h1 { margin: 0; }", "h1"),
            (":root { --x: 1px; }", ":root"),
            ("@media (min-width: 100px) { p { margin: 0; } }", "p"),
            ("body:has(.row) { margin: 0; }", "body:has(.row)"),
            (":where(.row, p) { margin: 0; }", ":where(.row, p)"),
            (":not(.row) { margin: 0; }", ":not(.row)"),
        ] {
            let faults = faults_of(css);
            assert_eq!(faults.len(), 1, "{css}: {faults:?}");
            assert!(
                faults[0].starts_with(&format!("the selector `{selector}` names no class")),
                "{css}: {faults:?}"
            );
        }
    }

    #[test]
    fn a_layer_is_refused() {
        for css in [
            "@layer extension { .row { margin: 0; } }",
            "@layer one, two;",
        ] {
            let faults = faults_of(css);
            assert_eq!(faults.len(), 1, "{css}: {faults:?}");
            assert!(faults[0].starts_with("`@layer`"), "{css}: {faults:?}");
        }
    }
}
