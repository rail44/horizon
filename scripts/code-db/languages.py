"""Per-language adapters: the only place that knows about a concrete language.

An adapter names the tree-sitter grammar for each file extension, one *unit
query* and a set of *tag queries*. Everything downstream (schema, loaders,
SQL) only sees the neutral vocabulary these queries produce:

* Unit query: every pattern captures one definition node as
  ``@unit.<kind>`` (``function``, ``method``, ``type``, ``module``, ``impl``,
  ``macro``, ...) and one or more ``@name`` nodes. Several ``@name`` captures
  are joined in source order with ``name_separator``. When patterns capture
  the same node with different kinds, the pattern written first wins, so
  list specific patterns (methods) before general ones (functions).
* Tag queries: ``tag -> query``; each pattern captures the tagged node as
  ``@tagged`` (other captures only serve predicates). A tag applies to the
  whole byte range of the tagged node; consumers inherit it by containment.
* File tags: ``tag -> path globs``; a matching file gets the tag on its root
  node. ``**/`` also matches at the repository root.
"""

from dataclasses import dataclass, field

import tree_sitter_go
import tree_sitter_rust
import tree_sitter_typescript


@dataclass(frozen=True)
class Adapter:
    name: str
    packages: tuple[str, ...]
    grammars: dict  # extension (with dot) -> callable returning a language capsule
    unit_query: str
    tag_queries: dict = field(default_factory=dict)
    file_tags: dict = field(default_factory=dict)
    name_separator: str = " "


RUST_ATTRIBUTE_FOLLOWERS = """[(attribute_item) (line_comment) (block_comment)]*"""
RUST_ITEMS = """[(function_item) (mod_item) (impl_item) (struct_item) (enum_item)
  (union_item) (trait_item) (type_item) (const_item) (static_item)
  (use_declaration) (macro_invocation) (macro_definition)]"""
RUST_CFG_TEST = """[
  (token_tree . (identifier) @t . (#eq? @t "test"))
  (token_tree . (identifier) @all (#eq? @all "all") . (token_tree (identifier) @t (#eq? @t "test")))
]"""

RUST = Adapter(
    name="rust",
    packages=("tree-sitter-rust",),
    grammars={".rs": tree_sitter_rust.language},
    name_separator=" for ",
    unit_query="""
(impl_item body: (declaration_list (function_item name: (identifier) @name) @unit.method))
(trait_item body: (declaration_list (function_item name: (identifier) @name) @unit.method))
(trait_item body: (declaration_list (function_signature_item name: (identifier) @name) @unit.method))
(function_item name: (identifier) @name) @unit.function
(impl_item trait: (_) @name type: (_) @name) @unit.impl
(impl_item !trait type: (_) @name) @unit.impl
(struct_item name: (_) @name) @unit.type
(enum_item name: (_) @name) @unit.type
(union_item name: (_) @name) @unit.type
(type_item name: (_) @name) @unit.type
(trait_item name: (_) @name) @unit.type
(mod_item name: (_) @name) @unit.module
(macro_definition name: (_) @name) @unit.macro
""",
    tag_queries={
        # `#[test]`, `#[tokio::test]`, `#[rstest]`, ... on the next item,
        # skipping further attributes and comments in between.
        "test": f"""
((attribute_item (attribute [(identifier) @a (scoped_identifier name: (identifier) @a)]
   (#any-of? @a "test" "rstest" "test_case" "bench")))
 . {RUST_ATTRIBUTE_FOLLOWERS} . (function_item) @tagged)
((attribute_item (attribute (identifier) @c (#eq? @c "cfg") arguments: {RUST_CFG_TEST}))
 . {RUST_ATTRIBUTE_FOLLOWERS} . {RUST_ITEMS} @tagged)
((source_file (inner_attribute_item (attribute (identifier) @c (#eq? @c "cfg") arguments: {RUST_CFG_TEST}))) @tagged)
""",
        "trait_impl": "(impl_item trait: (_)) @tagged",
        "generated_or_macro": "(macro_invocation) @tagged (macro_definition) @tagged",
    },
    file_tags={
        "test": ("**/tests/**", "**/tests.rs", "**/*_tests.rs", "**/test_*.rs"),
    },
)

GO_RECEIVER_TYPE = """[(type_identifier) @name
  (pointer_type (type_identifier) @name)
  (generic_type type: (type_identifier) @name)
  (pointer_type (generic_type type: (type_identifier) @name))]"""

GO = Adapter(
    name="go",
    packages=("tree-sitter-go",),
    grammars={".go": tree_sitter_go.language},
    name_separator=".",
    unit_query=f"""
(method_declaration
  receiver: (parameter_list (parameter_declaration type: {GO_RECEIVER_TYPE}))
  name: (field_identifier) @name) @unit.method
(function_declaration name: (identifier) @name) @unit.function
(type_spec name: (type_identifier) @name) @unit.type
(type_alias name: (type_identifier) @name) @unit.type
""",
    tag_queries={
        "test": """
((function_declaration name: (identifier) @n) @tagged
 (#match? @n "^(Test|Benchmark|Fuzz|Example)([^a-z]|$)"))
""",
        "generated_or_macro": """
((source_file (comment) @c (#match? @c "^// Code generated .* DO NOT EDIT\\\\.$")) @tagged)
""",
    },
    file_tags={"test": ("**/*_test.go",)},
)

TYPESCRIPT_UNITS = """
(method_definition name: (_) @name) @unit.method
(function_declaration name: (identifier) @name) @unit.function
(generator_function_declaration name: (identifier) @name) @unit.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @unit.function
(class_declaration name: (_) @name) @unit.type
(abstract_class_declaration name: (_) @name) @unit.type
(interface_declaration name: (_) @name) @unit.type
(type_alias_declaration name: (_) @name) @unit.type
(enum_declaration name: (_) @name) @unit.type
(internal_module name: (_) @name) @unit.module
"""

TYPESCRIPT = Adapter(
    name="typescript",
    packages=("tree-sitter-typescript",),
    grammars={
        ".ts": tree_sitter_typescript.language_typescript,
        ".tsx": tree_sitter_typescript.language_tsx,
    },
    name_separator=".",
    unit_query=TYPESCRIPT_UNITS,
    tag_queries={
        "test": """
(call_expression function: (identifier) @f (#any-of? @f "describe" "it" "test")) @tagged
(call_expression function: (member_expression object: (identifier) @f)
  (#any-of? @f "describe" "it" "test")) @tagged
""",
        "generated_or_macro": """
((program . (comment) @c (#match? @c "@generated|DO NOT EDIT")) @tagged)
""",
    },
    file_tags={
        "test": ("**/*.test.ts", "**/*.spec.ts", "**/*.test.tsx", "**/*.spec.tsx"),
    },
)

ADAPTERS = {adapter.name: adapter for adapter in (RUST, GO, TYPESCRIPT)}


def select(names):
    unknown = [name for name in names if name not in ADAPTERS]
    if unknown:
        raise ValueError(f"Unknown language(s) {unknown}; known: {sorted(ADAPTERS)}")
    return [ADAPTERS[name] for name in names]
