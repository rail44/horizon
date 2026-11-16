//! `#[plugin(..)]`: a plugin described once, in Rust.
//!
//! On a module, it reads what the module declares:
//!
//! - `struct State`: what a pane keeps of the plugin, a WIT record;
//! - `fn mount` and `fn state`: the view on a state, and the state now
//!   (a plugin whose panes keep nothing has no `State` and no `state`,
//!   and its `mount` takes nothing);
//! - a function marked `#[create(..)]`: what a new pane's state is made
//!   from, its parameters being what the host asks for or gives (a
//!   `File`, a `String` or an `Option<String>`, a `WorkingDirectory`);
//! - functions marked `#[command(..)]`: the plugin's commands.
//!
//! From those it writes the plugin's WIT world, hands it to `wit-bindgen`
//! for the bindings, exports the functions through them beside what
//! every plugin exports, and puts a manifest (the name, the title, each
//! command's title and description, how a parameter is asked for, what
//! may be read) in a custom section of the wasm. What crosses to the host
//! is the component's type and that section, as it is for a plugin whose
//! WIT and manifest are written by hand.
//!
//! `access(..)` holds what the plugin is let have that takes saying what
//! of: the directories it may read. Which of the host's interfaces it
//! uses is not said here: the component imports what its code calls, and
//! the host reads that.
//!
//! ```ignore
//! #[horizon_ui::plugin(name = "notes", title = "Notes", access(read = ["/"]))]
//! mod notes {
//!     pub struct State {
//!         pub path: Option<String>,
//!     }
//!
//!     pub fn mount(state: Option<State>) -> u64 { .. }
//!     pub fn state() -> State { .. }
//!
//!     #[create(file(extensions = ["md"]))]
//!     pub fn create(file: File) -> State { .. }
//!
//!     #[command(title = "Back", description = "Go back.")]
//!     pub fn back() { .. }
//! }
//! ```

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, ExprArray, ExprLit, FnArg, Item, ItemFn, ItemMod, ItemStruct, Lit, LitByteStr,
    LitStr, Pat, ReturnType, Type,
};

/// What `#[plugin(..)]` says of the plugin itself.
#[derive(Default)]
struct About {
    name: Option<String>,
    title: Option<String>,
    read: Vec<String>,
}

/// A command, as its attribute says it.
struct Command {
    function: syn::Ident,
    title: String,
    description: String,
    destructive: bool,
}

/// How one parameter of `create` is asked for.
struct Asked {
    parameter: String,
    extensions: Vec<String>,
}

fn error(span: Span, said: &str) -> syn::Error {
    syn::Error::new(span, said)
}

/// The strings of `["a", "b"]`.
fn strings(array: &ExprArray) -> syn::Result<Vec<String>> {
    (array.elems.iter())
        .map(|element| match element {
            Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) => Ok(text.value()),
            other => Err(error(other.span(), "a string")),
        })
        .collect()
}

/// A name as WIT writes it.
fn kebab(name: &str) -> String {
    name.replace('_', "-")
}

/// A Rust type as the WIT type that carries it.
fn wit(ty: &Type) -> syn::Result<String> {
    let none = || error(ty.span(), "a type the boundary has no way to carry");
    let Type::Path(path) = ty else {
        return Err(none());
    };
    let last = path.path.segments.last().ok_or_else(none)?;
    let inner = || match &last.arguments {
        syn::PathArguments::AngleBracketed(arguments) => match arguments.args.first() {
            Some(syn::GenericArgument::Type(inner)) => wit(inner),
            _ => Err(none()),
        },
        _ => Err(none()),
    };
    Ok(match last.ident.to_string().as_str() {
        "String" => "string".to_string(),
        "bool" => "bool".to_string(),
        "State" => "state".to_string(),
        "File" => "file".to_string(),
        "WorkingDirectory" => "working-directory".to_string(),
        "Option" => format!("option<{}>", inner()?),
        "Vec" => format!("list<{}>", inner()?),
        number @ ("u8" | "u16" | "u32" | "u64" | "s8" | "s16" | "s32" | "s64" | "f32" | "f64") => {
            number.to_string()
        }
        signed @ ("i8" | "i16" | "i32" | "i64") => signed.replacen('i', "s", 1),
        _ => return Err(none()),
    })
}

/// A function's parameters as (name, type), and its WIT signature.
fn signature(function: &ItemFn) -> syn::Result<(Vec<syn::Ident>, String)> {
    let mut names = Vec::new();
    let mut parameters = Vec::new();
    for argument in &function.sig.inputs {
        let FnArg::Typed(argument) = argument else {
            return Err(error(
                argument.span(),
                "a plugin's function takes no `self`",
            ));
        };
        let Pat::Ident(name) = &*argument.pat else {
            return Err(error(argument.pat.span(), "a parameter with a name"));
        };
        parameters.push(format!(
            "{}: {}",
            kebab(&name.ident.to_string()),
            wit(&argument.ty)?
        ));
        names.push(name.ident.clone());
    }
    let result = match &function.sig.output {
        ReturnType::Default => String::new(),
        ReturnType::Type(_, ty) => format!(" -> {}", wit(ty)?),
    };
    Ok((names, format!("func({}){result}", parameters.join(", "))))
}

/// Takes the attribute named `name` out of `attributes`.
fn take(attributes: &mut Vec<Attribute>, name: &str) -> Option<Attribute> {
    let at = (attributes.iter()).position(|attribute| attribute.path().is_ident(name))?;
    Some(attributes.remove(at))
}

/// The export that hands `function` on, with the bindings' own types.
fn forward(function: &ItemFn, names: &[syn::Ident]) -> Tokens {
    let name = &function.sig.ident;
    let inputs = &function.sig.inputs;
    let output = &function.sig.output;
    quote! {
        fn #name(#inputs) #output {
            #name(#(#names),*)
        }
    }
}

fn expand(about: About, module: ItemMod) -> syn::Result<Tokens> {
    let name = about
        .name
        .ok_or_else(|| error(Span::call_site(), "`name = \"..\"`"))?;
    let title = about
        .title
        .ok_or_else(|| error(Span::call_site(), "`title = \"..\"`"))?;
    let (_, items) = module
        .content
        .ok_or_else(|| error(module.ident.span(), "a module with its items in it"))?;

    let mut kept = Vec::new();
    let mut record = None::<ItemStruct>;
    let mut world = Vec::new();
    let mut world_functions = Vec::new();
    let mut commands = Vec::new();
    let mut command_lines = Vec::new();
    let mut command_functions = Vec::new();
    let mut asked = Vec::new();
    let (mut mounts, mut states) = (false, false);

    for item in items {
        match item {
            Item::Struct(declared) if declared.ident == "State" => record = Some(declared),
            Item::Fn(mut function) => {
                // (Only what is exported crosses: a function of the
                // module's own takes whatever it likes.)
                let exported = (function.attrs.iter()).any(|attribute| {
                    attribute.path().is_ident("command") || attribute.path().is_ident("create")
                }) || function.sig.ident == "mount"
                    || function.sig.ident == "state";
                if !exported {
                    kept.push(Item::Fn(function));
                    continue;
                }
                let (names, shape) = signature(&function)?;
                let written = kebab(&function.sig.ident.to_string());
                if let Some(attribute) = take(&mut function.attrs, "command") {
                    let (mut title, mut description, mut destructive) = (None, None, false);
                    attribute.parse_nested_meta(|meta| {
                        if meta.path.is_ident("title") {
                            title = Some(meta.value()?.parse::<LitStr>()?.value());
                        } else if meta.path.is_ident("description") {
                            description = Some(meta.value()?.parse::<LitStr>()?.value());
                        } else if meta.path.is_ident("destructive") {
                            destructive = true;
                        } else {
                            return Err(meta.error("`title`, `description` or `destructive`"));
                        }
                        Ok(())
                    })?;
                    let said = |what: Option<String>, of: &str| {
                        what.ok_or_else(|| error(attribute.span(), of))
                    };
                    commands.push(Command {
                        function: function.sig.ident.clone(),
                        title: said(title, "a command's `title`")?,
                        description: said(description, "a command's `description`")?,
                        destructive,
                    });
                    command_lines.push(format!("        {written}: {shape};"));
                    command_functions.push(forward(&function, &names));
                } else if let Some(attribute) = take(&mut function.attrs, "create") {
                    // (How each parameter is asked for: `file(extensions = [..])`.)
                    if !matches!(attribute.meta, syn::Meta::Path(_)) {
                        attribute.parse_nested_meta(|meta| {
                            let parameter = (meta.path.get_ident())
                                .ok_or_else(|| meta.error("a parameter's name"))?
                                .to_string();
                            let mut extensions = Vec::new();
                            meta.parse_nested_meta(|meta| {
                                if meta.path.is_ident("extensions") {
                                    extensions = strings(&meta.value()?.parse::<ExprArray>()?)?;
                                    Ok(())
                                } else {
                                    Err(meta.error("`extensions`"))
                                }
                            })?;
                            if !names.iter().any(|name| *name == parameter) {
                                return Err(meta.error("`create` has no such parameter"));
                            }
                            asked.push(Asked {
                                parameter: kebab(&parameter),
                                extensions,
                            });
                            Ok(())
                        })?;
                    }
                    if function.sig.ident != "create" {
                        return Err(error(function.sig.ident.span(), "named `create`"));
                    }
                    world.push(format!("    export create: {shape};"));
                    world_functions.push(forward(&function, &names));
                } else if function.sig.ident == "mount" {
                    mounts = true;
                    world.push(format!("    export mount: {shape};"));
                    world_functions.push(forward(&function, &names));
                } else if function.sig.ident == "state" {
                    states = true;
                    world.push(format!("    export state: {shape};"));
                    world_functions.push(forward(&function, &names));
                }
                kept.push(Item::Fn(function));
            }
            other => kept.push(other),
        }
    }
    // (The record is the bindings' own: what is derived for `State` is
    // derived for that.)
    let mut derives = Vec::<syn::Path>::new();
    for attribute in record.iter().flat_map(|record| &record.attrs) {
        if attribute.path().is_ident("derive") {
            attribute.parse_nested_meta(|meta| {
                derives.push(meta.path);
                Ok(())
            })?;
        }
    }
    // A plugin keeps a state and says it, or keeps none.
    match (&record, mounts, states) {
        (Some(_), true, true) | (None, true, false) => {}
        (Some(_), ..) => return Err(error(module.ident.span(), "`fn mount` and `fn state`")),
        (None, ..) => {
            return Err(error(
                module.ident.span(),
                "`fn mount`, and with a `struct State` a `fn state`",
            ));
        }
    }

    // The world, as a plugin's own WIT would say it.
    let mut declared = String::new();
    if let Some(record) = &record {
        let mut fields = Vec::new();
        for field in &record.fields {
            let name =
                (field.ident.as_ref()).ok_or_else(|| error(field.span(), "a field with a name"))?;
            fields.push(format!(
                "        {}: {},",
                kebab(&name.to_string()),
                wit(&field.ty)?
            ));
        }
        declared = format!("    record state {{\n{}\n    }}\n\n", fields.join("\n"));
    }
    let package = kebab(&name);
    let mut text = format!(
        "package horizon-ext:{package};\n\nworld plugin {{\n    include horizon:ui/imports;\n    \
         use horizon:ui/files.{{file, working-directory}};\n\n{declared}{}\n",
        world.join("\n")
    );
    if !command_lines.is_empty() {
        text.push_str(&format!(
            "\n    export commands: interface {{\n{}\n    }}\n",
            command_lines.join("\n")
        ));
    }
    text.push_str("}\n");

    // The manifest: what the component's type does not carry, as JSON.
    let described: serde_json::Map<String, serde_json::Value> = (commands.iter())
        .map(|command| {
            let said = serde_json::json!({
                "title": command.title,
                "description": command.description,
                "destructive": command.destructive,
            });
            (kebab(&command.function.to_string()), said)
        })
        .collect();
    let created: serde_json::Map<String, serde_json::Value> = (asked.iter())
        .map(|asked| {
            let said = serde_json::json!({ "extensions": asked.extensions });
            (asked.parameter.to_string(), said)
        })
        .collect();
    let manifest = serde_json::json!({
        "plugin": { "name": name, "title": title },
        "commands": described,
        "create": created,
        "access": { "read": about.read },
    })
    .to_string();
    let length = manifest.len();
    let manifest = LitByteStr::new(manifest.as_bytes(), Span::call_site());

    // (The boundary's own WIT is the SDK's, beside this crate.)
    let host = concat!(env!("CARGO_MANIFEST_DIR"), "/../horizon-ui/wit");
    let world_name = format!("horizon-ext:{package}/plugin");
    let commands_export = (!command_functions.is_empty()).then(|| {
        quote! {
            impl exports::commands::Guest for HorizonPlugin {
                #(#command_functions)*
            }
        }
    });
    let (visibility, ident, attributes) = (&module.vis, &module.ident, &module.attrs);
    Ok(quote! {
        #(#attributes)*
        #visibility mod #ident {
            ::wit_bindgen::generate!({
                inline: #text,
                path: #host,
                world: #world_name,
                // What every plugin imports is the SDK's own bindings.
                with: {
                    "horizon:ui/document": ::horizon_ui::guest::bindings::horizon::ui::document,
                    "horizon:ui/desktop": ::horizon_ui::guest::bindings::horizon::ui::desktop,
                    "horizon:ui/pane": ::horizon_ui::guest::bindings::horizon::ui::pane,
                    "horizon:ui/files": ::horizon_ui::guest::bindings::horizon::ui::files,
                    "horizon:ui/theme": ::horizon_ui::guest::bindings::horizon::ui::theme,
                },
                additional_derives: [#(#derives),*],
            });

            #(#kept)*

            struct HorizonPlugin;

            impl Guest for HorizonPlugin {
                #(#world_functions)*
            }

            #commands_export

            export!(HorizonPlugin);
            // What every plugin exports.
            ::horizon_ui::export_view!();

            /// The manifest, as a custom section of the wasm.
            #[unsafe(link_section = "horizon:manifest")]
            #[used]
            static HORIZON_MANIFEST: [u8; #length] = *#manifest;
        }
    })
}

/// Expands `#[plugin(..)]` on a module.
pub(crate) fn plugin(arguments: TokenStream, input: TokenStream) -> TokenStream {
    let mut about = About::default();
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") {
            about.name = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("title") {
            about.title = Some(meta.value()?.parse::<LitStr>()?.value());
        } else if meta.path.is_ident("access") {
            meta.parse_nested_meta(|meta| {
                if meta.path.is_ident("read") {
                    about.read = strings(&meta.value()?.parse::<ExprArray>()?)?;
                    Ok(())
                } else {
                    Err(meta.error("`read`"))
                }
            })?;
        } else {
            return Err(meta.error("`name`, `title` or `access(..)`"));
        }
        Ok(())
    });
    syn::parse_macro_input!(arguments with parser);
    let module = syn::parse_macro_input!(input as ItemMod);
    match expand(about, module) {
        Ok(expanded) => expanded.into(),
        Err(error) => error.to_compile_error().into(),
    }
}
