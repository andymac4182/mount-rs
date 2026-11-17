//! Reading an OpenAPI document and checking responses against it.
//!
//! Shared by the conformance suites of this crate and by the tests of
//! `mount-rs-cli` that check a real `serve-http` process, including one
//! with cluster features, against the same documents.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::io::Read;

use serde_json::{Map, Value, json};

pub const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"];

/// Headers every response may carry without the document repeating them
/// under each response. `x-request-id` is checked on all of them instead.
pub const UNLISTED_HEADERS: [&str; 4] = ["content-type", "content-length", "date", "x-request-id"];

/// Parse a document: YAML with string keys.
pub fn parse(document: &str) -> Value {
    serde_yaml::from_str(document).expect("the document is YAML with string keys")
}

/// Follow `$ref` until the value is not a reference.
pub fn resolve<'a>(doc: &'a Value, mut value: &'a Value) -> &'a Value {
    while let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .unwrap_or_else(|| panic!("only local references are supported: {reference}"));
        value = doc
            .pointer(pointer)
            .unwrap_or_else(|| panic!("dangling reference {reference}"));
    }
    value
}

#[derive(Clone)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub template: String,
    pub spec: Value,
}

impl Operation {
    pub fn declared(&self) -> BTreeSet<u16> {
        self.spec["responses"]
            .as_object()
            .expect("responses")
            .keys()
            .map(|status| status.parse().expect("numeric status"))
            .collect()
    }

    pub fn on_a_drive(&self) -> bool {
        self.template.contains("{id}")
    }
}

pub fn operations(doc: &Value) -> Vec<Operation> {
    let mut found = Vec::new();
    for (template, item) in doc["paths"].as_object().expect("paths") {
        for method in METHODS {
            if let Some(spec) = item.get(method.to_ascii_lowercase()) {
                found.push(Operation {
                    id: spec["operationId"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{method} {template} has no operationId"))
                        .to_owned(),
                    method: method.to_owned(),
                    template: template.clone(),
                    spec: spec.clone(),
                });
            }
        }
    }
    let ids: BTreeSet<_> = found.iter().map(|operation| &operation.id).collect();
    assert_eq!(ids.len(), found.len(), "operationIds are unique");
    found
}

/// The document path a concrete request path belongs to. `{path}` stands
/// for one or more segments, any other parameter for exactly one.
pub fn template_of<'a>(doc: &'a Value, path: &str) -> Option<&'a str> {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    doc["paths"]
        .as_object()
        .expect("paths")
        .keys()
        .find(|template| {
            let pattern: Vec<&str> = template.trim_start_matches('/').split('/').collect();
            let rest = pattern.last() == Some(&"{path}");
            if segments.len() < pattern.len() || (!rest && segments.len() != pattern.len()) {
                return false;
            }
            pattern
                .iter()
                .zip(&segments)
                .all(|(pattern, segment)| pattern.starts_with('{') || pattern == segment)
        })
        .map(String::as_str)
}

// ------------------------------------------------------------ the validator

/// Check `value` against `schema`, for the part of JSON Schema the
/// document uses. A keyword this does not implement is an error, so a
/// schema is never passed by being ignored.
pub fn validate(
    doc: &Value,
    schema: &Value,
    value: &Value,
    at: &str,
) -> std::result::Result<(), String> {
    let rules = schema
        .as_object()
        .ok_or_else(|| format!("{at}: schema is not an object"))?;
    for (keyword, rule) in rules {
        match keyword.as_str() {
            "$ref" => validate(doc, resolve(doc, schema), value, at)?,
            "description" | "example" | "examples" | "default" | "format" => {}
            "type" => {
                let allowed: Vec<&str> = match rule {
                    Value::String(name) => vec![name],
                    Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
                    _ => return Err(format!("{at}: unreadable type")),
                };
                let is = |name: &str| match name {
                    "string" => value.is_string(),
                    "integer" => value.is_i64() || value.is_u64(),
                    "number" => value.is_number(),
                    "boolean" => value.is_boolean(),
                    "object" => value.is_object(),
                    "array" => value.is_array(),
                    "null" => value.is_null(),
                    _ => false,
                };
                if !allowed.iter().any(|name| is(name)) {
                    return Err(format!("{at}: {value} is not of type {rule}"));
                }
            }
            "const" => {
                if value != rule {
                    return Err(format!("{at}: {value} is not {rule}"));
                }
            }
            "enum" => {
                if !rule.as_array().is_some_and(|values| values.contains(value)) {
                    return Err(format!("{at}: {value} is not one of {rule}"));
                }
            }
            "required" => {
                if let Some(object) = value.as_object() {
                    for name in rule.as_array().into_iter().flatten() {
                        let name = name.as_str().unwrap_or_default();
                        if !object.contains_key(name) {
                            return Err(format!("{at}: `{name}` is missing"));
                        }
                    }
                }
            }
            "properties" => {
                if let Some(object) = value.as_object() {
                    for (name, schema) in rule.as_object().into_iter().flatten() {
                        if let Some(value) = object.get(name) {
                            validate(doc, schema, value, &format!("{at}.{name}"))?;
                        }
                    }
                }
            }
            "additionalProperties" => {
                let known = rules.get("properties").and_then(Value::as_object);
                for (name, value) in value.as_object().into_iter().flatten() {
                    if known.is_some_and(|known| known.contains_key(name)) {
                        continue;
                    }
                    match rule {
                        Value::Bool(true) => {}
                        Value::Bool(false) => {
                            return Err(format!("{at}: `{name}` is not allowed"));
                        }
                        schema => validate(doc, schema, value, &format!("{at}.{name}"))?,
                    }
                }
            }
            "items" => {
                for (index, value) in value.as_array().into_iter().flatten().enumerate() {
                    validate(doc, rule, value, &format!("{at}[{index}]"))?;
                }
            }
            "minItems" | "maxItems" => {
                if let Some(items) = value.as_array() {
                    let bound = rule.as_u64().ok_or("unreadable bound")? as usize;
                    let ok = if keyword == "minItems" {
                        items.len() >= bound
                    } else {
                        items.len() <= bound
                    };
                    if !ok {
                        return Err(format!(
                            "{at}: {} items breaks {keyword} {bound}",
                            items.len()
                        ));
                    }
                }
            }
            "minLength" | "maxLength" => {
                if let Some(text) = value.as_str() {
                    let bound = rule.as_u64().ok_or("unreadable bound")? as usize;
                    let length = text.chars().count();
                    let ok = if keyword == "minLength" {
                        length >= bound
                    } else {
                        length <= bound
                    };
                    if !ok {
                        return Err(format!("{at}: length {length} breaks {keyword} {bound}"));
                    }
                }
            }
            "minimum" | "maximum" => {
                if value.is_number() {
                    // Exact for every integer JSON can hold here.
                    let number = |value: &Value| {
                        value
                            .as_i64()
                            .map(i128::from)
                            .or_else(|| value.as_u64().map(i128::from))
                    };
                    let (Some(actual), Some(bound)) = (number(value), number(rule)) else {
                        return Err(format!("{at}: {keyword} is only implemented for integers"));
                    };
                    let ok = if keyword == "minimum" {
                        actual >= bound
                    } else {
                        actual <= bound
                    };
                    if !ok {
                        return Err(format!("{at}: {actual} breaks {keyword} {bound}"));
                    }
                }
            }
            "pattern" => {
                if let Some(text) = value.as_str() {
                    let pattern = rule.as_str().ok_or("unreadable pattern")?;
                    let regex = regex::Regex::new(pattern)
                        .map_err(|error| format!("{at}: pattern {pattern}: {error}"))?;
                    if !regex.is_match(text) {
                        return Err(format!("{at}: {text:?} does not match {pattern}"));
                    }
                }
            }
            "oneOf" | "anyOf" => {
                let branches = rule.as_array().ok_or("unreadable branches")?;
                let errors: Vec<String> = branches
                    .iter()
                    .filter_map(|branch| validate(doc, branch, value, at).err())
                    .collect();
                let matched = branches.len() - errors.len();
                if matched == 0 || (keyword == "oneOf" && matched != 1) {
                    return Err(format!(
                        "{at}: matches {matched} of {} {keyword} branches: {errors:?}",
                        branches.len()
                    ));
                }
            }
            other => {
                return Err(format!(
                    "{at}: the conformance validator does not implement `{other}`"
                ));
            }
        }
    }
    Ok(())
}

/// A header value as the JSON value its schema describes.
pub fn header_value(doc: &Value, schema: &Value, text: &str) -> Value {
    let integer = resolve(doc, schema).get("type").and_then(Value::as_str) == Some("integer");
    match text.parse::<i64>() {
        Ok(number) if integer => json!(number),
        _ => json!(text),
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn message(&self) -> Option<String> {
        let body: Value = serde_json::from_slice(&self.body).ok()?;
        Some(body["error"]["message"].as_str()?.to_owned())
    }

    pub fn collect(status: u16, headers: &reqwest::header::HeaderMap, body: &[u8]) -> Self {
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().expect("visible header").to_owned(),
                )
            })
            .collect();
        let gzip = headers
            .iter()
            .any(|(name, value)| name == "content-encoding" && value == "gzip");
        let body = if gzip && !body.is_empty() {
            let mut plain = Vec::new();
            flate2::read::GzDecoder::new(body)
                .read_to_end(&mut plain)
                .expect("gzip body");
            plain
        } else {
            body.to_vec()
        };
        Self {
            status,
            headers,
            body,
        }
    }
}

/// What is wrong with `reply` as an instance of the declared response.
pub fn check_response(doc: &Value, declared: &Value, method: &str, reply: &Reply) -> Vec<String> {
    let mut failures = Vec::new();
    let declared = resolve(doc, declared);
    let empty = Map::new();
    let headers = declared
        .get("headers")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    for (name, header) in headers {
        let header = resolve(doc, header);
        match reply.header(&name.to_ascii_lowercase()) {
            Some(value) => {
                let schema = &header["schema"];
                if let Err(error) = validate(doc, schema, &header_value(doc, schema, value), name) {
                    failures.push(format!("header {error}"));
                }
            }
            None if header.get("required") == Some(&Value::Bool(true)) => {
                failures.push(format!("required header `{name}` is missing"));
            }
            None => {}
        }
    }
    for (name, _) in &reply.headers {
        let listed = headers
            .keys()
            .any(|header| header.eq_ignore_ascii_case(name));
        if !listed && !UNLISTED_HEADERS.contains(&name.as_str()) {
            failures.push(format!("header `{name}` is sent but not declared"));
        }
    }
    let request_id = &doc["components"]["headers"]["XRequestId"]["schema"];
    match reply.header("x-request-id") {
        Some(id) => {
            if let Err(error) = validate(doc, request_id, &json!(id), "X-Request-Id") {
                failures.push(format!("header {error}"));
            }
        }
        None => failures.push("no `x-request-id` header".to_owned()),
    }

    let content = declared.get("content").and_then(Value::as_object);
    let media_type = reply.header("content-type").map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    });
    match (content, &media_type) {
        (Some(content), Some(media_type)) => match content.get(media_type) {
            None => failures.push(format!(
                "Content-Type {media_type} is not one of {:?}",
                content.keys().collect::<Vec<_>>()
            )),
            Some(media) if media_type == "application/json" && method != "HEAD" => {
                match serde_json::from_slice::<Value>(&reply.body) {
                    Ok(body) => {
                        if let Err(error) = validate(doc, &media["schema"], &body, "body") {
                            failures.push(error);
                        }
                    }
                    Err(error) => failures.push(format!("body is not JSON: {error}")),
                }
            }
            Some(_) => {}
        },
        (Some(_), None) => failures.push("no Content-Type on a response with content".to_owned()),
        (None, Some(media_type)) if !reply.body.is_empty() => failures.push(format!(
            "a {media_type} body where the document declares none"
        )),
        (None, _) => {}
    }
    if method == "HEAD" && !reply.body.is_empty() {
        failures.push("a HEAD response has a body".to_owned());
    }
    failures
}

pub fn report(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "the server and openapi.yaml disagree in {} places:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Words to try in every position of a path: every literal segment of the
/// document, and names a route the document does not have might go by.
pub fn vocabulary(doc: &Value) -> BTreeSet<String> {
    let mut words: BTreeSet<String> = doc["paths"]
        .as_object()
        .expect("paths")
        .keys()
        .flat_map(|template| template.split('/'))
        .filter(|segment| !segment.is_empty() && !segment.starts_with('{'))
        .map(str::to_owned)
        .collect();
    for extra in [
        "v2",
        "health",
        "ready",
        "livez",
        "status",
        "version",
        "metrics",
        "admin",
        "debug",
        "internal",
        "docs",
        "openapi.yaml",
        "sessions",
        "session",
        "forward",
        "cluster",
        "copy",
        "move",
        "delete",
        "list",
        "exists",
        "read",
        "write",
        "open",
        "close",
        "watch",
        "lock",
        "chmod",
        "chown",
        "utimes",
        "symlink",
        "link",
        "mknod",
        "unlink",
        "rmdir",
        "readdir",
    ] {
        words.insert(extra.to_owned());
    }
    words
}

/// Every `example` beside a `schema` in the document, with where it is.
pub fn examples<'a>(value: &'a Value, at: String, found: &mut Vec<(String, &'a Value, &'a Value)>) {
    match value {
        Value::Object(object) => {
            if let Some(schema) = object.get("schema") {
                if let Some(example) = object.get("example") {
                    found.push((at.clone(), schema, example));
                }
                for (name, example) in object
                    .get("examples")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                {
                    found.push((format!("{at}/examples/{name}"), schema, &example["value"]));
                }
            }
            for (name, child) in object {
                examples(child, format!("{at}/{name}"), found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                examples(child, format!("{at}/{index}"), found);
            }
        }
        _ => {}
    }
}
