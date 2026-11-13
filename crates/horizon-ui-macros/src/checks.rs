//! What a CSS module is held to, beyond being CSS: its values come from
//! the theme.
//!
//! - A color is one of the theme's variables, not written out
//!   (`transparent` and `currentColor` are not the theme's to give).
//! - A `font-size` or a `line-height` is not a number of pixels: the type
//!   scale's variables have those.
//! - A variable read with nothing to fall back on is the theme's or one
//!   the sheet sets itself. (One read with a fallback is how a part takes
//!   what whoever places it may set.)
//! - Nothing is `!important`.

use std::collections::HashSet;
use std::convert::Infallible;

use lightningcss::declaration::DeclarationBlock;
use lightningcss::properties::Property;
use lightningcss::properties::custom::{CustomPropertyName, Variable};
use lightningcss::properties::font::{FontSize, LineHeight};
use lightningcss::stylesheet::{PrinterOptions, StyleSheet};
use lightningcss::traits::ToCss;
use lightningcss::values::color::CssColor;
use lightningcss::values::length::{LengthPercentage, LengthValue};
use lightningcss::visit_types;
use lightningcss::visitor::{Visit, VisitTypes, Visitor};

/// The theme's variables, as `horizon-theme-css` sets them: the file is
/// that crate's, and it checks that it sets these and no others.
const THEME: &str = include_str!("../../horizon-theme-css/variables.txt");

#[derive(Default)]
struct Checks {
    /// The property being looked into, as the sheet names it.
    property: String,
    set: HashSet<String>,
    read: Vec<String>,
    faults: Vec<String>,
}

impl Checks {
    fn color(&mut self, color: &CssColor) {
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
            Property::Custom(custom) => {
                if let CustomPropertyName::Custom(name) = &custom.name {
                    self.set.insert(name.0.to_string());
                }
            }
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

/// What in `sheet` is not as a CSS module is to be written, each said in
/// a line.
pub(crate) fn faults(sheet: &mut StyleSheet) -> Vec<String> {
    let mut checks = Checks::default();
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
