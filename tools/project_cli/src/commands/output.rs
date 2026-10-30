use serde_json::Value;

/// Stable process status for machine automation: validation failures are 2,
/// compatibility failures are 3, and command/internal failures are 1.
pub(crate) fn exit_code(output: &Value) -> i32 {
    if has_incompatibility(output) {
        3
    } else if output["validation"]["ok"] == false || has_validation_failure(output) {
        2
    } else {
        i32::from(output["ok"] == false)
    }
}

fn has_incompatibility(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.get("compatible") == Some(&Value::Bool(false))
                || fields.values().any(has_incompatibility)
        }
        Value::Array(values) => values.iter().any(has_incompatibility),
        _ => false,
    }
}

fn has_validation_failure(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            (fields
                .get("errors")
                .is_some_and(|errors| errors.as_array().is_some_and(|errors| !errors.is_empty())))
                || fields.values().any(has_validation_failure)
        }
        Value::Array(values) => values.iter().any(has_validation_failure),
        _ => false,
    }
}

pub(crate) fn render_human_output(output: &Value) -> String {
    if matches!(output["command"].as_str(), Some("build" | "run" | "test")) {
        let command = output["command"].as_str().unwrap_or("");
        let mut text = format!(
            "Project {command}: {}",
            if output["ok"] == true {
                "passed"
            } else {
                "failed"
            }
        );
        if let Some(cargo) = output["cargo"].as_object() {
            for key in ["stdout", "stderr"] {
                if let Some(contents) = cargo[key].as_str().filter(|s| !s.is_empty()) {
                    text.push('\n');
                    text.push_str(contents.trim_end());
                }
            }
        } else {
            for error in output["validation"]["errors"]
                .as_array()
                .into_iter()
                .flatten()
            {
                text.push_str("\n- ");
                text.push_str(error.as_str().unwrap_or(""));
            }
        }
        return text;
    }
    if output["command"] == "new" {
        let ok = output["ok"].as_bool().unwrap_or(false);
        let path = output["path"].as_str().unwrap_or(".");
        let template = output["template"].as_str().unwrap_or("unknown");
        let name = output["name"].as_str().unwrap_or("unknown");
        let empty_files = Vec::new();
        let files = output["files"].as_array().unwrap_or(&empty_files);
        if ok {
            let mut lines = vec![format!(
                "Created project '{name}' from template '{template}' at {path}"
            )];
            for file in files {
                lines.push(format!("- {}", file.as_str().unwrap_or("")));
            }
            return lines.join("\n");
        }
        return format!(
            "Project creation failed: {}",
            output["message"].as_str().unwrap_or("unknown error")
        );
    }

    if output["command"] == "content hash" {
        let sha = output["sha256"].as_str().unwrap_or("unknown");
        return format!("SHA-256: {sha}");
    }

    if output["command"] == "content build" {
        return render_content_build(output);
    }

    if output["command"] == "content diff" {
        return render_content_diff(output);
    }

    if output["command"] == "content publish-check" {
        return render_publish_check(output);
    }

    if output["command"] == "doctor" {
        let ok = output["ok"].as_bool().unwrap_or(false);
        let backend = output["backend"].as_object();
        let project = output["project"].as_object();
        let mut lines = vec![format!(
            "Doctor status: {}",
            if ok { "ok" } else { "issues found" }
        )];
        if let Some(backend) = backend {
            lines.push(format!(
                "- backend: {} ({})",
                backend["healthy"],
                backend["message"].as_str().unwrap_or("")
            ));
            lines.push(format!("- schema: {}", backend["schema"]));
        }
        if let Some(project) = project {
            lines.push(format!(
                "- project: {} ({} errors, {} warnings)",
                project["ok"],
                project["errors"].as_array().map_or(0, Vec::len),
                project["warnings"].as_array().map_or(0, Vec::len)
            ));
        }
        return lines.join("\n");
    }

    if output["ok"].as_bool().unwrap_or(false) {
        let path = output["path"].as_str().unwrap_or(".");
        return format!("Validation passed for {path}");
    }

    let errors = output["errors"].as_array().map_or(0, Vec::len);
    format!("Validation failed with {errors} error(s)")
}

fn render_content_build(output: &Value) -> String {
    format!(
        "Content build {}: {}",
        if output["ok"] == true {
            "passed"
        } else {
            "failed"
        },
        output["output"].as_str().unwrap_or("manifest not written")
    )
}

fn render_content_diff(output: &Value) -> String {
    let ok = output["ok"].as_bool().unwrap_or(false);
    let base = output["base_version"].as_str().unwrap_or("unknown");
    let next = output["next_version"].as_str().unwrap_or("unknown");
    let actual = output["actual_bump"].as_str().unwrap_or("unknown");
    let required = output["required_bump"].as_str().unwrap_or("unknown");
    let mut lines = vec![format!(
        "Content diff {base} -> {next}: {} (actual {actual} bump, required {required} bump)",
        if ok { "compatible" } else { "incompatible" }
    )];
    for key in ["added", "removed"] {
        for entry in output[key].as_array().into_iter().flatten() {
            if let Some(id) = entry["id"].as_str() {
                lines.push(format!("- {key}: {id}"));
            }
        }
    }
    for entry in output["changed"].as_array().into_iter().flatten() {
        if let Some(id) = entry["id"].as_str() {
            lines.push(format!("- changed: {id}"));
        }
    }
    for key in ["added_locales", "removed_locales", "changed_locales"] {
        for locale in output[key].as_array().into_iter().flatten() {
            if let Some(locale) = locale.as_str() {
                lines.push(format!("- {key}: {locale}"));
            }
        }
    }
    push_diagnostics(&mut lines, &output["report"]["diagnostics"]);
    lines.join("\n")
}

fn render_publish_check(output: &Value) -> String {
    let ok = output["ok"].as_bool().unwrap_or(false);
    let author = output["author"].as_str().unwrap_or("unknown");
    let state = output["state"].as_str().unwrap_or("unknown");
    let revision = output["revision"].as_u64().unwrap_or(0);
    let mut lines = vec![format!(
        "Publish check for {author} revision {revision} ({state}): {}",
        if ok { "allowed" } else { "blocked" }
    )];
    push_diagnostics(&mut lines, &output["diagnostics"]);
    lines.join("\n")
}

fn push_diagnostics(lines: &mut Vec<String>, diagnostics: &Value) {
    for diagnostic in diagnostics.as_array().into_iter().flatten() {
        lines.push(format!(
            "- {} {}: {}",
            diagnostic["severity"].as_str().unwrap_or("?"),
            diagnostic["code"].as_str().unwrap_or("?"),
            diagnostic["message"].as_str().unwrap_or("")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::exit_code;
    use serde_json::json;

    #[test]
    fn automation_exit_codes_distinguish_validation_incompatibility_and_failure() {
        assert_eq!(exit_code(&json!({"ok": true})), 0);
        assert_eq!(
            exit_code(&json!({"ok": false, "validation": {"ok": false}})),
            2
        );
        assert_eq!(exit_code(&json!({"ok": false, "compatible": false})), 3);
        assert_eq!(
            exit_code(&json!({"ok": false, "schema": {"compatible": false}})),
            3
        );
        assert_eq!(exit_code(&json!({"ok": false})), 1);
    }
}
