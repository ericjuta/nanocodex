use std::fmt::Write as _;

use nanocodex_core::JsonSchema;

const EXEC_DESCRIPTION: &str = r#"Run Clojure code to orchestrate and compose tool calls
- Evaluates the provided Clojure forms in a fresh namespace on a prewarmed cljrs isolate. Yielded cells keep running independently until completion or termination.
- Call a nested tool with `(tool-call "tool_name" input)`; it returns a Clojure future. Use `(await ...)` to obtain its result. The equivalent namespaced primitive `(nanocodex.tools/call "tool_name" input)` remains supported.
- Start independent calls before awaiting them, then compose them with `(await (clojure.core.async/join-all [first second]))`. A failed nested tool rejects its future; `join-all` short-circuits and propagates the first failure. Use `(join-all futures {:on-error :cancel-pending})` to cancel siblings, `(join-all-settled futures)` for ordered fulfilled/rejected maps, `(race futures {:cancel-losers true})`, or `(await-with-timeout future ms)`. `alts` also propagates a winning failure. Catch expected failures with `try` and `(catch :default error ...)`.

Handle an expected tool failure without failing the cell:
```clojure
(try
  (await (tool-call "view_image" {:path "missing.png"}))
  (catch :default error
    (text {:message (ex-message error)
           :data (ex-data error)})))
```
- Nested tool failures carry `:type :nested-tool-failure`, `:tool`, `:input`, `:call-id`, and `:output` in `ex-data`.
- Tool inputs and results cross the host boundary as JSON-compatible Clojure values: maps, vectors, strings, numbers, booleans, and nil. Function tools normally take keyword-keyed maps such as `{:cmd "pwd"}`. JSON object results become keyword-keyed maps, so read fields with forms such as `(:output result)`.
- A cell's final value is not emitted. `print`, `println`, and related ambient output functions are unavailable; use `(text value)` or an image helper to return content.

A complete sequential call:
```clojure
(let [result (await (tool-call "exec_command" {:cmd "pwd" :login false}))]
  (text (:output result)))
```

A complete parallel call:
```clojure
(let [first (tool-call "exec_command" {:cmd "pwd" :login false})
      second (tool-call "exec_command" {:cmd "git status --short" :login false})
      results (await (clojure.core.async/join-all [first second]))]
  (text {:first (:output (nth results 0))
         :second (:output (nth results 1))}))
```
- Ambient filesystem, process, network, namespace loading, host interop, and module loading are unavailable. Use the provided tools for those capabilities.
- For normal UTF-8 file edits, prefer `hashline__read`/`hashline__find_block` plus `hashline__patch`; use `hashline__transaction` for recoverable multi-file batches.
- Hashline paths may be absolute or relative to the configured workspace. For routine patches outside it, set `:root` and keep section and `MV` paths root-relative; this is lexical scoping only. For transactions outside it, set `:root` and keep mutations root-relative.
- Accepts raw Clojure source text, not JSON, a quoted string, or a markdown code fence.
- You may optionally start the input with a first-line pragma like `;; @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}`.
- `yield_time_ms` asks `exec` to yield early if the cell is still running. Defaults to 10000 ms.
- `max_output_tokens` sets the token budget for direct `exec` results. Defaults to 10000 tokens.

Helpers available in every cell:
- `(tool-call name input)`: starts a nested tool call and returns a future.
- `(text value)`: appends a text item. Strings are emitted directly; JSON-compatible values are encoded as JSON.
- `(image image-url-or-map & [detail])`: appends an image item. The URL must be a base64 `data:` URI; detail is one of `:auto`, `:low`, `:high`, or `:original` (strings are also accepted).
- `(generated-image {:image_url data-uri :output_hint optional-string})`: appends an image-generation result and optional hint.
- `(notify value)`: immediately emits a notification for the current `exec` call.
- `(store key value)` and `(load key)`: persist JSON-compatible values across `exec` calls in the same session. Missing keys load as nil.
- `(await (yield-control))`: yields accumulated output while the cell keeps running; pending nested tools continue in the background.
- `(exit)`: completes the current cell successfully.
- `(all-tools & [query])`: returns enabled tool metadata. A string filters by name/description; a map accepts `:query`, `:kind`, `:dynamic`, `:limit`, `:cursor`, and `:include-schema`.
- `(tool-info name)`: returns one tool's metadata and schemas, or nil when unknown.
- `(pending-tools)`: returns metadata for this cell's unsettled nested calls, including id, state, and elapsed time.
- `(cancel-tool future-or-id)`: cancels that nested call and returns true, or false if it is no longer pending.
- `(tool-status future-or-id)` / `(await-tool future-or-id)`: inspect or re-acquire a pending nested-call handle.
- `(await (with-tool-scope {:on-error :cancel-pending :on-exit :cancel-pending} (fn [] ...)))`: owns nested calls created in the thunk and applies cancel/keep policy.
- `(code-mode-info)`: returns runtime versions, supported async forms/combinators, and active budgets.
- `(await (clojure.core.async/timeout milliseconds))`: cooperatively waits without blocking the isolate."#;

const MAX_EXEC_DESCRIPTION_BYTES: usize = 39_000;
const MAX_TOOL_SECTION_BYTES: usize = 8_000;
const MAX_SCHEMA_DETAIL_BYTES: usize = 2_000;
const MAX_SHAPE_BYTES: usize = 1_200;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 2_000;

pub(super) fn exec_description(definitions: &[nanocodex_core::ToolDefinition]) -> String {
    let mut description = EXEC_DESCRIPTION.to_owned();
    let mut omitted = 0_usize;
    for spec in definitions {
        let (input_shape, input_example, input_schema) = match spec {
            nanocodex_core::ToolDefinition::Function { .. } => {
                let schema = spec.parameters().map(JsonSchema::as_value);
                (
                    schema.map_or_else(
                        || "any".to_owned(),
                        |schema| bounded_utf8(&render_clojure_shape(schema), MAX_SHAPE_BYTES),
                    ),
                    schema.and_then(render_clojure_example),
                    schema,
                )
            }
            nanocodex_core::ToolDefinition::Custom { .. } => {
                ("string".to_owned(), Some(r#""input""#.to_owned()), None)
            }
        };
        let output_schema = spec.output_schema().map(JsonSchema::as_value);
        let output_shape = output_schema.map_or_else(
            || "unspecified JSON-compatible value".to_owned(),
            |schema| bounded_utf8(&render_clojure_shape(schema), MAX_SHAPE_BYTES),
        );
        let name = serde_json::to_string(spec.name()).unwrap_or_else(|_| r#""tool""#.to_owned());
        let call = input_example.map_or_else(
            || {
                format!(
                    "Clojure call template (not runnable until `input` is replaced with a value matching the schema):\n```clojure\n(await (tool-call {name} input))\n```"
                )
            },
            |input_example| {
                format!(
                    "Clojure call:\n```clojure\n(await (tool-call {name} {input_example}))\n```"
                )
            },
        );
        let mut section = String::new();
        let _ = write!(
            section,
            "\n\n### `{}`\n{}\n\n{call}\nClojure input shape (`optional` marks keys that may be omitted): `{input_shape}`\nClojure result shape: `{output_shape}`",
            spec.name(),
            bounded_utf8(spec.description().trim(), MAX_TOOL_DESCRIPTION_BYTES),
        );
        if let Some(input_schema) = input_schema {
            append_schema_detail(&mut section, "input", input_schema, &name);
        }
        if let Some(output_schema) = output_schema {
            append_schema_detail(&mut section, "output", output_schema, &name);
        }
        if section.len() > MAX_TOOL_SECTION_BYTES {
            section = format!(
                "\n\n### `{}`\nTool documentation exceeded the per-tool prompt budget; inspect full metadata with `(tool-info {name})`.",
                bounded_utf8(spec.name(), 128),
            );
        }
        if description
            .len()
            .saturating_add(section.len())
            .saturating_add(160)
            <= MAX_EXEC_DESCRIPTION_BYTES
        {
            description.push_str(&section);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted > 0 {
        let notice = format!(
            "\n\n{omitted} additional tool definition(s) were omitted to keep this prompt bounded. Discover them with `(all-tools)` and inspect one with `(tool-info name)`."
        );
        if description.len().saturating_add(notice.len()) <= MAX_EXEC_DESCRIPTION_BYTES {
            description.push_str(&notice);
        }
    }
    description
}

fn append_schema_detail(
    section: &mut String,
    label: &str,
    schema: &serde_json::Value,
    tool_name: &str,
) {
    let schema = compact_json(schema);
    if schema.len() <= MAX_SCHEMA_DETAIL_BYTES {
        let _ = write!(section, "\nDetailed {label} schema (JSON): `{schema}`");
    } else {
        let _ = write!(
            section,
            "\nDetailed {label} schema omitted ({} bytes); inspect `(tool-info {tool_name})`.",
            schema.len(),
        );
    }
}

fn bounded_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let suffix = "…";
    let target = max_bytes.saturating_sub(suffix.len());
    let boundary = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= target)
        .last()
        .unwrap_or(0);
    format!("{}{suffix}", &value[..boundary])
}

fn render_clojure_shape(schema: &serde_json::Value) -> String {
    let serde_json::Value::Object(schema) = schema else {
        return match schema {
            serde_json::Value::Bool(false) => "never".to_owned(),
            _ => "any".to_owned(),
        };
    };
    if let Some(value) = schema.get("const") {
        return render_clojure_literal(value);
    }
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        return values
            .iter()
            .map(render_clojure_literal)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    for combinator in ["oneOf", "anyOf"] {
        if let Some(variants) = schema.get(combinator).and_then(serde_json::Value::as_array) {
            let siblings = schema_siblings(schema, combinator);
            return variants
                .iter()
                .map(|variant| render_clojure_shape(&merge_schema_variant(&siblings, variant)))
                .collect::<Vec<_>>()
                .join(" | ");
        }
    }
    if let Some(variants) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        let mut combined = serde_json::Value::Object(schema_siblings(schema, "allOf"));
        for variant in variants {
            let serde_json::Value::Object(base) = combined else {
                return "any".to_owned();
            };
            combined = merge_schema_variant(&base, variant);
        }
        return render_clojure_shape(&combined);
    }
    if let Some(types) = schema.get("type").and_then(serde_json::Value::as_array) {
        return types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|schema_type| render_clojure_type(schema, schema_type))
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(schema_type) = schema.get("type").and_then(serde_json::Value::as_str) {
        return render_clojure_type(schema, schema_type);
    }
    if schema.contains_key("properties") || schema.contains_key("required") {
        return render_clojure_object(schema);
    }
    if schema.contains_key("items") || schema.contains_key("prefixItems") {
        return render_clojure_array(schema);
    }
    "any".to_owned()
}

fn schema_siblings(
    schema: &serde_json::Map<String, serde_json::Value>,
    combinator: &str,
) -> serde_json::Map<String, serde_json::Value> {
    schema
        .iter()
        .filter(|(key, _)| key.as_str() != combinator)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn merge_schema_variant(
    siblings: &serde_json::Map<String, serde_json::Value>,
    variant: &serde_json::Value,
) -> serde_json::Value {
    let serde_json::Value::Object(variant) = variant else {
        return variant.clone();
    };
    let mut combined = siblings.clone();
    for (key, value) in variant {
        match key.as_str() {
            "required" => {
                let mut required = combined
                    .get("required")
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for field in value.as_array().into_iter().flatten() {
                    if !required.contains(field) {
                        required.push(field.clone());
                    }
                }
                combined.insert(key.clone(), serde_json::Value::Array(required));
            }
            "properties" => {
                let mut properties = combined
                    .get("properties")
                    .and_then(serde_json::Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(variant_properties) = value.as_object() {
                    properties.extend(variant_properties.clone());
                    combined.insert(key.clone(), serde_json::Value::Object(properties));
                }
            }
            _ => {
                combined.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::Value::Object(combined)
}

fn render_clojure_type(
    schema: &serde_json::Map<String, serde_json::Value>,
    schema_type: &str,
) -> String {
    match schema_type {
        "string" => "string".to_owned(),
        "integer" => "integer".to_owned(),
        "number" => "number".to_owned(),
        "boolean" => "boolean".to_owned(),
        "null" => "nil".to_owned(),
        "array" => render_clojure_array(schema),
        "object" => render_clojure_object(schema),
        _ => "any".to_owned(),
    }
}

fn render_clojure_object(schema: &serde_json::Map<String, serde_json::Value>) -> String {
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    let mut properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    properties.sort_unstable_by_key(|(name, _)| *name);
    let mut fields = properties
        .into_iter()
        .map(|(name, value)| {
            let shape = render_clojure_shape(value);
            let shape = if required.contains(&name.as_str()) {
                shape
            } else {
                format!("(optional {shape})")
            };
            format!("{} {shape}", render_clojure_key(name))
        })
        .collect::<Vec<_>>();
    match schema.get("additionalProperties") {
        Some(serde_json::Value::Bool(true)) => fields.push("string-key any".to_owned()),
        Some(serde_json::Value::Object(_)) => fields.push(format!(
            "string-key {}",
            render_clojure_shape(&schema["additionalProperties"])
        )),
        _ => {}
    }
    format!("{{{}}}", fields.join(", "))
}

fn render_clojure_array(schema: &serde_json::Map<String, serde_json::Value>) -> String {
    if let Some(items) = schema.get("items") {
        return format!("[{} ...]", render_clojure_shape(items));
    }
    if let Some(items) = schema
        .get("prefixItems")
        .and_then(serde_json::Value::as_array)
    {
        return format!(
            "[{}]",
            items
                .iter()
                .map(render_clojure_shape)
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    "[any ...]".to_owned()
}

const MAX_SYNTHESIZED_ITEMS: usize = 16;
const MAX_SYNTHESIZED_STRING_CHARS: usize = 256;
const MAX_SYNTHESIZED_DEPTH: usize = 32;
const MAX_SYNTHESIZED_BYTES: usize = 1_024;

fn render_clojure_example(schema: &serde_json::Value) -> Option<String> {
    example_value(schema).as_ref().map(render_clojure_literal)
}

pub(crate) fn example_value(schema: &serde_json::Value) -> Option<serde_json::Value> {
    let mut references = Vec::new();
    let value = example_value_inner(schema, schema, 0, &mut references)?;
    let encoded = serde_json::to_vec(&value).ok()?;
    (encoded.len() <= MAX_SYNTHESIZED_BYTES && jsonschema::validate(schema, &value).is_ok())
        .then_some(value)
}

fn example_value_inner(
    schema: &serde_json::Value,
    root: &serde_json::Value,
    depth: usize,
    references: &mut Vec<String>,
) -> Option<serde_json::Value> {
    if depth >= MAX_SYNTHESIZED_DEPTH {
        return None;
    }
    let serde_json::Value::Object(schema) = schema else {
        return match schema {
            serde_json::Value::Bool(true) => Some(serde_json::Value::Null),
            _ => None,
        };
    };
    let current = serde_json::Value::Object(schema.clone());

    if let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
        if !reference.starts_with('#') || references.iter().any(|active| active == reference) {
            return None;
        }
        let target = root.pointer(reference.strip_prefix('#')?)?;
        let siblings = schema_siblings(schema, "$ref");
        let resolved = if siblings.is_empty() {
            target.clone()
        } else if let serde_json::Value::Object(target) = target {
            merge_schema_variant(target, &serde_json::Value::Object(siblings))
        } else if target == &serde_json::Value::Bool(true) {
            serde_json::Value::Object(siblings)
        } else {
            return None;
        };
        references.push(reference.to_owned());
        let candidate = example_value_inner(&resolved, root, depth.saturating_add(1), references);
        references.pop();
        return candidate;
    }

    if let Some(value) = annotated_candidates(&current)
        .into_iter()
        .find(|value| schema_accepts(root, &current, value))
    {
        return Some(value);
    }

    for combinator in ["oneOf", "anyOf"] {
        if let Some(variants) = schema.get(combinator).and_then(serde_json::Value::as_array) {
            let siblings = schema_siblings(schema, combinator);
            for variant in variants {
                let branch = merge_schema_variant(&siblings, variant);
                let mut candidates = annotated_candidates(&branch);
                if let Some(candidate) =
                    example_value_inner(&branch, root, depth.saturating_add(1), references)
                    && !candidates.contains(&candidate)
                {
                    candidates.push(candidate);
                }
                for candidate in candidates {
                    if schema_accepts(root, &branch, &candidate)
                        && schema_accepts(root, &current, &candidate)
                    {
                        return Some(candidate);
                    }
                }
            }
            return None;
        }
    }
    if let Some(variants) = schema.get("allOf").and_then(serde_json::Value::as_array) {
        let mut combined = serde_json::Value::Object(schema_siblings(schema, "allOf"));
        for variant in variants {
            let serde_json::Value::Object(base) = combined else {
                return None;
            };
            combined = merge_schema_variant(&base, variant);
        }
        return example_value_inner(&combined, root, depth.saturating_add(1), references)
            .filter(|candidate| schema_accepts(root, &current, candidate));
    }

    for schema_type in declared_and_inferred_types(schema) {
        let candidate = match schema_type {
            "string" => string_example(schema),
            "integer" => integer_example(schema),
            "number" => number_example(schema),
            "boolean" => Some(serde_json::Value::Bool(false)),
            "null" => Some(serde_json::Value::Null),
            "array" => array_example(schema, root, depth.saturating_add(1), references),
            "object" => object_example(schema, root, depth.saturating_add(1), references),
            _ => None,
        };
        if let Some(candidate) =
            candidate.filter(|candidate| schema_accepts(root, &current, candidate))
        {
            return Some(candidate);
        }
    }
    None
}

fn declared_and_inferred_types(schema: &serde_json::Map<String, serde_json::Value>) -> Vec<&str> {
    let mut types = schema
        .get("type")
        .map_or_else(Vec::new, |value| match value {
            serde_json::Value::String(value) => vec![value.as_str()],
            serde_json::Value::Array(values) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect(),
            _ => Vec::new(),
        });
    if schema.contains_key("properties") || schema.contains_key("required") {
        types.push("object");
    } else if schema.contains_key("items") || schema.contains_key("prefixItems") {
        types.push("array");
    }
    types
}

fn annotated_candidates(schema: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(schema) = schema.as_object() else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    if let Some(value) = schema.get("const") {
        candidates.push(value.clone());
    }
    if let Some(values) = schema.get("examples").and_then(serde_json::Value::as_array) {
        candidates.extend(values.iter().cloned());
    }
    if let Some(value) = schema.get("default") {
        candidates.push(value.clone());
    }
    if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
        candidates.extend(values.iter().cloned());
    }
    candidates.truncate(MAX_SYNTHESIZED_ITEMS);
    candidates
}

fn schema_accepts(
    root: &serde_json::Value,
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> bool {
    if jsonschema::validate(schema, value).is_ok() {
        return true;
    }
    let (serde_json::Value::Object(root), serde_json::Value::Object(schema)) = (root, schema)
    else {
        return false;
    };
    let mut envelope = schema.clone();
    for key in ["$defs", "definitions"] {
        if !envelope.contains_key(key)
            && let Some(definitions) = root.get(key)
        {
            envelope.insert(key.to_owned(), definitions.clone());
        }
    }
    jsonschema::validate(&serde_json::Value::Object(envelope), value).is_ok()
}

fn string_example(
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    if schema.contains_key("pattern") {
        return None;
    }
    let minimum = schema
        .get("minLength")
        .and_then(serde_json::Value::as_u64)
        .map_or(Some(0), |value| usize::try_from(value).ok())?;
    let maximum = schema
        .get("maxLength")
        .and_then(serde_json::Value::as_u64)
        .map_or(Some(MAX_SYNTHESIZED_STRING_CHARS), |value| {
            usize::try_from(value).ok()
        })?;
    if minimum > maximum || minimum > MAX_SYNTHESIZED_STRING_CHARS {
        return None;
    }
    let target = minimum.max(5.min(maximum));
    let mut value = "value".chars().take(target).collect::<String>();
    value.extend(std::iter::repeat_n(
        'x',
        minimum.saturating_sub(value.chars().count()),
    ));
    Some(serde_json::Value::String(value))
}

fn integer_example(
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let minimum = schema.get("minimum").map_or(Some(i64::MIN), ceil_i64)?;
    let exclusive_minimum = schema
        .get("exclusiveMinimum")
        .map_or(Some(i64::MIN), |value| floor_i64(value)?.checked_add(1))?;
    let maximum = schema.get("maximum").map_or(Some(i64::MAX), floor_i64)?;
    let exclusive_maximum = schema
        .get("exclusiveMaximum")
        .map_or(Some(i64::MAX), |value| ceil_i64(value)?.checked_sub(1))?;
    let lower = minimum.max(exclusive_minimum);
    let upper = maximum.min(exclusive_maximum);
    if lower > upper {
        return None;
    }
    let mut value = if lower <= 0 && upper >= 0 {
        0
    } else if upper < 0 {
        upper
    } else {
        lower
    };
    if let Some(multiple) = schema.get("multipleOf").and_then(serde_json::Value::as_i64) {
        if multiple <= 0 {
            return None;
        }
        let remainder = value.rem_euclid(multiple);
        if value < 0 {
            value = value.checked_sub(remainder)?;
        } else if remainder != 0 {
            value = value.checked_add(multiple - remainder)?;
        }
    }
    (value >= lower && value <= upper).then(|| serde_json::Value::Number(value.into()))
}

fn ceil_i64(value: &serde_json::Value) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).ok();
    }
    value.as_f64()?.ceil().to_string().parse().ok()
}

fn floor_i64(value: &serde_json::Value) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    if let Some(value) = value.as_u64() {
        return i64::try_from(value).ok();
    }
    value.as_f64()?.floor().to_string().parse().ok()
}

fn number_example(
    schema: &serde_json::Map<String, serde_json::Value>,
) -> Option<serde_json::Value> {
    let minimum = schema
        .get("minimum")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(f64::NEG_INFINITY);
    let maximum = schema
        .get("maximum")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(f64::INFINITY);
    let exclusive_minimum = schema
        .get("exclusiveMinimum")
        .and_then(serde_json::Value::as_f64);
    let exclusive_maximum = schema
        .get("exclusiveMaximum")
        .and_then(serde_json::Value::as_f64);
    let lower = exclusive_minimum.unwrap_or(minimum);
    let upper = exclusive_maximum.unwrap_or(maximum);
    let mut value = if 0.0 < lower {
        lower
    } else if 0.0 > upper {
        upper
    } else {
        0.0
    };
    if exclusive_minimum.is_some_and(|bound| value <= bound) {
        value = if upper.is_finite() {
            value.midpoint(upper)
        } else {
            value + 1.0
        };
    }
    if exclusive_maximum.is_some_and(|bound| value >= bound) {
        value = if lower.is_finite() {
            value.midpoint(lower)
        } else {
            value - 1.0
        };
    }
    if let Some(multiple) = schema.get("multipleOf").and_then(serde_json::Value::as_f64) {
        if multiple <= 0.0 {
            return None;
        }
        value = if value < 0.0 {
            (value / multiple).floor() * multiple
        } else {
            (value / multiple).ceil() * multiple
        };
    }
    if value < minimum
        || value > maximum
        || exclusive_minimum.is_some_and(|bound| value <= bound)
        || exclusive_maximum.is_some_and(|bound| value >= bound)
    {
        return None;
    }
    serde_json::Number::from_f64(value).map(serde_json::Value::Number)
}

fn array_example(
    schema: &serde_json::Map<String, serde_json::Value>,
    root: &serde_json::Value,
    depth: usize,
    references: &mut Vec<String>,
) -> Option<serde_json::Value> {
    if schema.contains_key("contains") {
        return None;
    }
    let minimum = schema
        .get("minItems")
        .and_then(serde_json::Value::as_u64)
        .map_or(Some(0), |value| usize::try_from(value).ok())?;
    let maximum = schema
        .get("maxItems")
        .and_then(serde_json::Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .ok()?;
    if minimum > MAX_SYNTHESIZED_ITEMS {
        return None;
    }
    let mut values = Vec::new();
    for item in schema
        .get("prefixItems")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        values.push(example_value_inner(item, root, depth, references)?);
    }
    if values.len() > MAX_SYNTHESIZED_ITEMS || maximum.is_some_and(|maximum| values.len() > maximum)
    {
        return None;
    }
    while values.len() < minimum {
        let value = match schema.get("items") {
            Some(serde_json::Value::Bool(false)) => return None,
            Some(items) => example_value_inner(items, root, depth, references)?,
            None => serde_json::Value::Null,
        };
        values.push(value);
    }
    if schema
        .get("uniqueItems")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        && values
            .iter()
            .enumerate()
            .any(|(index, value)| values[..index].contains(value))
    {
        return None;
    }
    Some(serde_json::Value::Array(values))
}

fn object_example(
    schema: &serde_json::Map<String, serde_json::Value>,
    root: &serde_json::Value,
    depth: usize,
    references: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let properties = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    let mut value = serde_json::Map::new();
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if required.len() > MAX_SYNTHESIZED_ITEMS {
        return None;
    }
    for name in required {
        let field = match properties.and_then(|properties| properties.get(name)) {
            Some(schema) => example_value_inner(schema, root, depth, references)?,
            None if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) => {
                return None;
            }
            None => serde_json::Value::Null,
        };
        value.insert(name.to_owned(), field);
    }
    let minimum = schema
        .get("minProperties")
        .and_then(serde_json::Value::as_u64)
        .map_or(Some(0), |value| usize::try_from(value).ok())?;
    if value.len() < minimum {
        let mut optional = properties
            .into_iter()
            .flat_map(|properties| properties.iter())
            .filter(|(name, _)| !value.contains_key(*name))
            .collect::<Vec<_>>();
        optional.sort_unstable_by_key(|(name, _)| *name);
        for (name, schema) in optional {
            value.insert(
                name.clone(),
                example_value_inner(schema, root, depth, references)?,
            );
            if value.len() == minimum {
                break;
            }
        }
    }
    let maximum = schema
        .get("maxProperties")
        .and_then(serde_json::Value::as_u64)
        .map(usize::try_from)
        .transpose()
        .ok()?;
    if value.len() < minimum || maximum.is_some_and(|maximum| value.len() > maximum) {
        return None;
    }
    Some(serde_json::Value::Object(value))
}

fn render_clojure_key(name: &str) -> String {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-?!*+".contains(character))
    {
        format!(":{name}")
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| r#""key""#.to_owned())
    }
}

fn render_clojure_literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "nil".to_owned(),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(render_clojure_literal)
                .collect::<Vec<_>>()
                .join(" ")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(name, value)| format!(
                    "{} {}",
                    render_clojure_key(name),
                    render_clojure_literal(value)
                ))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        _ => compact_json(value),
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use nanocodex_core::{CustomToolFormat, ToolDefinition};

    use super::{
        EXEC_DESCRIPTION, example_value, exec_description, render_clojure_example,
        render_clojure_shape,
    };

    #[test]
    fn renders_clojure_shape_with_keyword_keys_and_optional_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "choice": {"type": "string", "enum": ["one", "two"]},
                "count": {"type": "integer"}
            },
            "required": ["choice"],
            "additionalProperties": false
        });
        assert_eq!(
            render_clojure_shape(&schema),
            r#"{:choice "one" | "two", :count (optional integer)}"#
        );
    }

    #[test]
    fn combines_shared_properties_with_one_of_variants() {
        let schema = json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string"},
                "header": {"type": "string"},
                "operations": {"type": "string"}
            },
            "oneOf": [
                {"required": ["patch"]},
                {"required": ["header", "operations"]}
            ],
            "additionalProperties": false
        });
        assert_eq!(
            render_clojure_shape(&schema),
            "{:header (optional string), :operations (optional string), :patch string} | {:header string, :operations string, :patch (optional string)}"
        );
        assert_eq!(
            render_clojure_example(&schema).as_deref(),
            Some(r#"{:patch "value"}"#)
        );
    }

    #[test]
    fn guidance_covers_cljrs_boundaries_and_hashline_roots() {
        assert!(EXEC_DESCRIPTION.contains("Catch expected failures"));
        assert!(EXEC_DESCRIPTION.contains("final value is not emitted"));
        assert!(EXEC_DESCRIPTION.contains("For routine patches outside it"));
        assert!(EXEC_DESCRIPTION.contains("section and `MV` paths root-relative"));
        assert!(EXEC_DESCRIPTION.contains("lexical scoping only"));
    }

    #[test]
    fn synthesized_examples_honor_examples_bounds_arrays_and_unions() {
        let schemas = [
            (
                json!({"type": "string", "minLength": 8, "examples": ["curated-example"]}),
                json!("curated-example"),
            ),
            (json!({"type": "integer", "minimum": 7}), json!(7)),
            (
                json!({"type": "array", "items": {"type": "integer", "minimum": 2}, "minItems": 3}),
                json!([2, 2, 2]),
            ),
            (
                json!({"type": "array", "prefixItems": [{"type": "string"}, {"type": "integer"}], "minItems": 2}),
                json!(["value", 0]),
            ),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "patch": {"type": "string"},
                        "header": {"type": "string"}
                    },
                    "oneOf": [
                        {"required": ["patch"], "not": {"required": ["header"]}},
                        {"required": ["header"], "not": {"required": ["patch"]}}
                    ],
                    "additionalProperties": false
                }),
                json!({"patch": "value"}),
            ),
        ];
        for (schema, expected) in schemas {
            let value = example_value(&schema).expect("supported schema should synthesize");
            assert_eq!(value, expected);
            jsonschema::validate(&schema, &value).expect("synthesized example should validate");
        }
    }

    #[test]
    fn synthesis_validates_annotations_backtracks_unions_and_resolves_local_refs() {
        let invalid_annotations = json!({
            "type": "string",
            "minLength": 5,
            "examples": [1, "x"],
            "default": "no"
        });
        assert_eq!(example_value(&invalid_annotations), Some(json!("value")));

        let exclusive_union = json!({
            "oneOf": [
                {"type": "integer", "enum": [0, 1]},
                {"const": 0}
            ]
        });
        assert_eq!(example_value(&exclusive_union), Some(json!(1)));

        let referenced_all_of = json!({
            "$defs": {
                "base": {
                    "type": "object",
                    "properties": {"a": {"type": "integer", "minimum": 2}},
                    "required": ["a"]
                }
            },
            "allOf": [
                {"$ref": "#/$defs/base"},
                {
                    "type": "object",
                    "properties": {"b": {"type": "string"}},
                    "required": ["b"]
                }
            ]
        });
        let value = example_value(&referenced_all_of).expect("local ref should synthesize");
        assert_eq!(value, json!({"a": 2, "b": "value"}));
        jsonschema::validate(&referenced_all_of, &value)
            .expect("referenced example should validate");

        assert!(example_value(&json!({"$ref": "#/$defs/missing"})).is_none());
        assert!(
            example_value(&json!({
                "$defs": {"loop": {"$ref": "#/$defs/loop"}},
                "$ref": "#/$defs/loop"
            }))
            .is_none()
        );
    }

    #[test]
    fn synthesis_handles_negative_multiples() {
        let integer = json!({
            "type": "integer",
            "minimum": -5,
            "maximum": -1,
            "multipleOf": 2
        });
        assert_eq!(example_value(&integer), Some(json!(-2)));
        assert!(example_value(&json!({"type": "integer", "multipleOf": -2})).is_none());
    }

    #[test]
    fn generated_description_marks_placeholders_and_enforces_budgets() {
        let impossible = ToolDefinition::function(
            "impossible",
            "No value can satisfy this schema.",
            json!(false),
        );
        let oversized = ToolDefinition::function(
            "oversized",
            "界".repeat(2_000),
            json!({
                "type": "object",
                "description": "x".repeat(3_000),
                "additionalProperties": false
            }),
        );
        let description = exec_description(&[impossible, oversized]);
        assert!(description.contains("Clojure call template (not runnable"));
        assert!(description.contains("Detailed input schema omitted"));
        assert!(description.len() <= 39_000);
        assert!(std::str::from_utf8(description.as_bytes()).is_ok());

        let many = (0..100)
            .map(|index| {
                ToolDefinition::function(
                    format!("bounded_{index}"),
                    "description ".repeat(100),
                    json!({"type": "object"}),
                )
            })
            .collect::<Vec<_>>();
        let bounded = exec_description(&many);
        assert!(bounded.len() <= 39_000);
        assert!(bounded.contains("additional tool definition(s) were omitted"));
        assert!(bounded.contains("(all-tools)"));
    }

    #[test]
    fn generated_description_omits_only_redundant_schema_lines() {
        let description = exec_description(&[
            ToolDefinition::custom(
                "freeform",
                "Accepts freeform input.",
                CustomToolFormat::grammar("lark", "start: /.+/"),
            ),
            ToolDefinition::function(
                "structured",
                "Accepts structured input.",
                json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                    "additionalProperties": false
                }),
            )
            .with_output_schema(json!({"type": "string"})),
        ]);

        assert_eq!(
            description.matches("Detailed input schema (JSON):").count(),
            1
        );
        assert_eq!(
            description
                .matches("Detailed output schema (JSON):")
                .count(),
            1
        );
        assert!(description.contains("Detailed output schema (JSON): `{"));
        assert!(
            !description
                .contains("Detailed output schema (JSON): unspecified JSON-compatible value")
        );
    }
}
