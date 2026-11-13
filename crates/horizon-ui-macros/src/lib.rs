//! `css_module!`: a view's CSS as a CSS module.
//!
//! The macro reads plain CSS at compile time. Every class the CSS names
//! (and every other name it declares, such as an animation's) is given a
//! name no other sheet has, and the place the macro is written gets one
//! constant per class:
//!
//! ```ignore
//! mod styles {
//!     horizon_ui::css_module! {"
//!         .select { position: relative;
//!             .trigger { display: flex; }
//!         }
//!     "}
//! }
//!
//! el("div").class(styles::SELECT).child(el("div").class(styles::TRIGGER));
//! ```
//!
//! A class the Rust code does not name is a constant never used, and one
//! the CSS does not have is a constant that does not exist. The sheet
//! goes into the document when one of its classes is first put on an
//! element.
//!
//! The CSS is also held to the theme (`checks.rs`): what is not is a
//! compile error.

mod checks;

use std::hash::{DefaultHasher, Hash, Hasher};

use lightningcss::css_modules::{Config, Pattern};
use lightningcss::stylesheet::{ParserOptions, PrinterOptions, StyleSheet};
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{LitStr, parse_macro_input};

/// See the crate's documentation.
#[proc_macro]
pub fn css_module(input: TokenStream) -> TokenStream {
    let source = parse_macro_input!(input as LitStr);
    match module(&source.value()) {
        Ok(tokens) => tokens.into(),
        Err(message) => syn::Error::new(source.span(), message)
            .to_compile_error()
            .into(),
    }
}

/// The constant a class is named by: `status-dot` is `STATUS_DOT`.
fn constant(class: &str) -> String {
    class
        .chars()
        .map(|character| match character {
            '-' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

fn module(css: &str) -> Result<proc_macro2::TokenStream, String> {
    // The sheet's name: the same CSS is the same sheet wherever it is
    // written.
    let mut hasher = DefaultHasher::new();
    css.hash(&mut hasher);
    let id = format!("{:08x}", hasher.finish() as u32);
    // A class keeps its own name in front, for whoever reads the document.
    let pattern = format!("[local]--{id}");
    let options = ParserOptions {
        filename: id.clone(),
        css_modules: Some(Config {
            pattern: Pattern::parse(&pattern).map_err(|error| error.to_string())?,
            dashed_idents: false,
            ..Config::default()
        }),
        ..ParserOptions::default()
    };
    let mut sheet = StyleSheet::parse(css, options).map_err(|error| error.to_string())?;
    let faults = checks::faults(&mut sheet);
    if !faults.is_empty() {
        return Err(faults.join("\n"));
    }
    let printed = sheet
        .to_css(PrinterOptions {
            minify: true,
            ..PrinterOptions::default()
        })
        .map_err(|error| error.to_string())?;
    let code = printed.code;
    let mut classes: Vec<(String, String)> = (printed.exports.unwrap_or_default().into_iter())
        .map(|(local, export)| (local, export.name))
        // The names that are classes: an animation's name is declared too.
        .filter(|(_, name)| code.contains(&format!(".{name}")))
        .collect();
    classes.sort();
    let mut constants = Vec::new();
    for (index, (local, name)) in classes.iter().enumerate() {
        let constant = constant(local);
        if syn::parse_str::<syn::Ident>(&constant).is_err() {
            return Err(format!("the class `{local}` cannot be named by a constant"));
        }
        if let Some((other, _)) =
            (classes[..index].iter()).find(|(other, _)| self::constant(other) == constant)
        {
            return Err(format!(
                "the classes `{other}` and `{local}` are both `{constant}`"
            ));
        }
        let constant = format_ident!("{constant}");
        let said = format!("The class `{local}`.");
        constants.push(quote! {
            #[doc = #said]
            pub const #constant: ::horizon_ui::Class = ::horizon_ui::Class::new(#name, &__SHEET);
        });
    }
    Ok(quote! {
        #[doc(hidden)]
        pub const __SHEET: ::horizon_ui::Sheet = ::horizon_ui::Sheet::new(#id, #code);
        #(#constants)*
    })
}

#[cfg(test)]
mod tests {
    use super::module;

    fn refused(css: &str) -> String {
        module(css).expect_err("refused").to_string()
    }

    #[test]
    fn a_sheet_written_from_the_theme_is_taken() {
        let made = module(
            ".row { color: var(--text); background: transparent; border: 1px solid currentColor;
                    font-size: var(--meta-size); line-height: 1.4; padding: var(--u4) 2px;
                    --inset: 4px; margin: var(--inset); gap: var(--gap, var(--u8)); }",
        );
        assert!(made.unwrap().to_string().contains("ROW"));
    }

    #[test]
    fn a_color_written_out_is_refused() {
        assert!(refused(".a { color: #fff; }").contains("`color` has the color"));
        assert!(refused(".a { border: 1px solid red; }").contains("written out"));
        assert!(refused(".a { box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2); }").contains("box-shadow"));
    }

    #[test]
    fn a_type_size_in_pixels_is_refused() {
        assert!(refused(".a { font-size: 12px; }").contains("`font-size`"));
        assert!(refused(".a { line-height: 20px; }").contains("`line-height`"));
    }

    #[test]
    fn a_variable_nobody_sets_is_refused_unless_it_has_a_fallback() {
        assert!(refused(".a { color: var(--surfce); }").contains("`var(--surfce)`"));
        assert!(module(".a { color: var(--theirs, var(--text)); }").is_ok());
    }

    #[test]
    fn a_property_css_does_not_have_is_refused() {
        assert!(refused(".a { colr: var(--text); }").contains("`colr` is not a property"));
        assert!(module(".a { text-wrap: balance; rotate: 90deg; }").is_ok());
    }

    #[test]
    fn important_is_refused() {
        assert!(refused(".a { width: 1px !important; }").contains("`!important`"));
    }
}
