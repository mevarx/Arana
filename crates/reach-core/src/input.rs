//! Data-driven argument parsing with a literal `--` boundary.
use crate::{ReachError, Result};
use serde_json::{json, Value};
fn bad(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}
pub struct Parsed {
    pub params: Value,
    pub timeout: f64,
    pub backend: Option<String>,
    pub verbose: bool,
}
pub fn validate(action: &Value, params: &Value, timeout: f64) -> Result<Value> {
    if !timeout.is_finite()
        || timeout <= 0.0
        || std::time::Duration::try_from_secs_f64(timeout).is_err()
    {
        return Err(bad("timeout must be finite and positive"));
    }
    let values = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    let specs = action["params"]
        .as_array()
        .ok_or_else(|| bad("Invalid action parameter registry"))?;
    let mut clean = json!({});
    for p in specs {
        let name = p["name"].as_str().unwrap_or("");
        let kind = p["type"].as_str().unwrap_or("str");
        let mut value = values.get(name).cloned().unwrap_or(Value::Null);
        if let Some(s) = value.as_str() {
            value = json!(s.trim());
        }
        if value.is_null() || value == "" {
            if p["required"] == true {
                return Err(bad(format!("missing required argument: {name}")));
            }
            value = p["default"].clone();
        } else if kind == "str" && !value.is_string() {
            return Err(bad(format!("{name} must be str")));
        } else if kind == "int" {
            let n = if value.is_boolean() {
                None
            } else {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
                    .or_else(|| {
                        value
                            .as_f64()
                            .filter(|f| {
                                f.fract() == 0.0 && *f >= i64::MIN as f64 && *f < (i64::MAX as f64)
                            })
                            .map(|f| f as i64)
                    })
            };
            value = json!(n.ok_or_else(|| bad(format!("{name} must be int")))?);
        }
        if kind == "int" && matches!(name, "limit" | "page") && value.as_i64().is_none_or(|n| n < 1)
        {
            return Err(bad(format!("{name} must be a positive integer")));
        }
        clean[name] = value;
    }
    let mut unknown: Vec<_> = values
        .keys()
        .filter(|k| !specs.iter().any(|p| p["name"].as_str() == Some(k.as_str())))
        .cloned()
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(bad(format!("unknown argument(s): {}", unknown.join(", "))));
    }
    Ok(clean)
}
pub fn parse(action: &Value, args: &[String]) -> Result<Parsed> {
    let specs = action["params"]
        .as_array()
        .ok_or_else(|| bad("Invalid action parameter registry"))?;
    let mut params = json!({});
    let mut positionals = vec![];
    let mut timeout = action["timeout"].as_f64().unwrap_or(30.0);
    let mut backend = None;
    let mut verbose = false;
    let mut literal = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !literal && a == "--" {
            literal = true;
            i += 1;
            continue;
        }
        if !literal && a == "--verbose" {
            verbose = true;
            i += 1;
            continue;
        }
        if !literal && a.starts_with("--") {
            let (name, inline) = a[2..]
                .split_once('=')
                .map(|(n, v)| (n, Some(v.to_owned())))
                .unwrap_or((&a[2..], None));
            if name != "timeout"
                && name != "backend"
                && !specs
                    .iter()
                    .any(|p| p["name"] == name && p["required"] != true)
            {
                return Err(bad(format!("unrecognized arguments: {a}")));
            }
            let value = if let Some(v) = inline {
                v
            } else {
                i += 1;
                args.get(i)
                    .filter(|v| v.as_str() != "--")
                    .cloned()
                    .ok_or_else(|| bad(format!("argument --{name}: expected one argument")))?
            };
            match name {
                "timeout" => {
                    timeout = value
                        .parse()
                        .map_err(|_| bad("timeout must be finite and positive"))?
                }
                "backend" => backend = Some(value),
                _ => params[name] = json!(value),
            }
        } else if !literal && a.starts_with('-') && a != "-" {
            return Err(bad(format!("unrecognized arguments: {a}")));
        } else {
            positionals.push(a.clone());
        }
        i += 1;
    }
    let required: Vec<_> = specs.iter().filter(|p| p["required"] == true).collect();
    if required.len() == 1 {
        if !positionals.is_empty() {
            let name = required[0]["name"].as_str().unwrap();
            params[name] = json!(positionals.join(" "));
        }
    } else if required.len() == positionals.len() {
        for (p, v) in required.iter().zip(positionals) {
            params[p["name"].as_str().unwrap()] = json!(v);
        }
    } else if !positionals.is_empty() {
        return Err(bad("unexpected positional arguments"));
    }
    Ok(Parsed {
        params: validate(action, &params, timeout)?,
        timeout,
        backend,
        verbose,
    })
}
