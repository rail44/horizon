//! JSON Pointer (RFC 6901) reads and upserting writes over the data model,
//! with A2UI's convention that `""` and `/` both name the whole model.

use serde_json::{Map, Value};

fn is_root(pointer: &str) -> bool {
    pointer.is_empty() || pointer == "/"
}

fn tokens(pointer: &str) -> impl Iterator<Item = String> + '_ {
    pointer
        .strip_prefix('/')
        .unwrap_or(pointer)
        .split('/')
        .map(|token| token.replace("~1", "/").replace("~0", "~"))
}

pub(crate) fn get<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    if is_root(pointer) {
        return Some(root);
    }
    tokens(pointer).try_fold(root, |node, token| match node {
        Value::Object(map) => map.get(&token),
        Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    })
}

/// Writes `value` at `pointer`, creating intermediate objects as needed.
/// `null` removes an object key instead of storing `null`, per the spec's
/// "set value explicitly to null to remove a key".
pub(crate) fn set(root: &mut Value, pointer: &str, value: Value) {
    if is_root(pointer) {
        *root = if value.is_null() {
            Value::Object(Map::new())
        } else {
            value
        };
        return;
    }
    let tokens: Vec<String> = tokens(pointer).collect();
    let (last, parents) = tokens.split_last().expect("non-root pointer has a token");
    let mut node = root;
    for token in parents {
        node = child_mut(node, token);
    }
    match node {
        Value::Array(items) => match last.parse::<usize>() {
            Ok(i) if i < items.len() => items[i] = value,
            Ok(i) if i == items.len() => items.push(value),
            _ if last == "-" => items.push(value),
            // Out-of-range or non-numeric index into an array: ignored.
            _ => {}
        },
        _ => {
            if !node.is_object() {
                *node = Value::Object(Map::new());
            }
            let map = node.as_object_mut().expect("just made an object");
            if value.is_null() {
                map.remove(last);
            } else {
                map.insert(last.clone(), value);
            }
        }
    }
}

fn child_mut<'a>(node: &'a mut Value, token: &str) -> &'a mut Value {
    let index = match (&*node, token.parse::<usize>()) {
        (Value::Array(items), Ok(i)) if i < items.len() => Some(i),
        _ => None,
    };
    if let Some(i) = index {
        return &mut node.as_array_mut().expect("checked above")[i];
    }
    if !node.is_object() {
        *node = Value::Object(Map::new());
    }
    node.as_object_mut()
        .expect("just made an object")
        .entry(token.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
}

/// Resolves a binding path against the enclosing template item's absolute
/// pointer (`""` outside any template).
pub(crate) fn join(scope: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        scope.to_string()
    } else {
        format!("{scope}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_objects_and_arrays() {
        let model = json!({"a": {"b": [10, {"c": "x"}]}, "s/l": 1, "t~": 2});
        assert_eq!(get(&model, "/a/b/0"), Some(&json!(10)));
        assert_eq!(get(&model, "/a/b/1/c"), Some(&json!("x")));
        assert_eq!(get(&model, "/s~1l"), Some(&json!(1)));
        assert_eq!(get(&model, "/t~0"), Some(&json!(2)));
        assert_eq!(get(&model, "/missing"), None);
        assert_eq!(get(&model, "/"), Some(&model));
    }

    #[test]
    fn set_upserts_and_creates_intermediates() {
        let mut model = json!({});
        set(&mut model, "/form/user/name", json!("Ada"));
        assert_eq!(model, json!({"form": {"user": {"name": "Ada"}}}));
        set(&mut model, "/form/user/name", json!("Grace"));
        assert_eq!(get(&model, "/form/user/name"), Some(&json!("Grace")));
    }

    #[test]
    fn set_appends_and_replaces_array_items() {
        let mut model = json!({"items": ["a"]});
        set(&mut model, "/items/1", json!("b"));
        set(&mut model, "/items/-", json!("c"));
        set(&mut model, "/items/0", json!("A"));
        assert_eq!(model, json!({"items": ["A", "b", "c"]}));
    }

    #[test]
    fn null_removes_a_key_and_root_replaces_everything() {
        let mut model = json!({"keep": 1, "drop": 2});
        set(&mut model, "/drop", Value::Null);
        assert_eq!(model, json!({"keep": 1}));
        set(&mut model, "/", json!({"fresh": true}));
        assert_eq!(model, json!({"fresh": true}));
    }

    #[test]
    fn join_resolves_relative_paths_against_the_template_item() {
        assert_eq!(join("/items/2", "name"), "/items/2/name");
        assert_eq!(join("/items/2", "/title"), "/title");
        assert_eq!(join("", "name"), "/name");
    }
}
