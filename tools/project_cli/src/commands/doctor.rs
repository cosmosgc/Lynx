use super::{absolute_path, project};
use crate::DoctorArgs;
use anyhow::Result;
use backend_nakama::{NakamaClient, NakamaConfig};
use game_api::HealthService;
use serde_json::{Value, json};
use std::path::Path;

pub(crate) fn doctor(args: &DoctorArgs) -> Result<Value> {
    let mut report = json!({
        "ok": true,
        "command": "doctor",
        "project": null,
        "backend": null,
    });

    let has_project = args.project.is_some();
    if let Some(project_path) = args.project.as_ref() {
        let dir = absolute_path(project_path.as_path())?;
        let project_status = project::validate_dir(dir.as_path())?;
        report["project"] = project_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["project"]["ok"].as_bool().unwrap(),
        );
    }

    if args.backend {
        let backend_status = check_backend();
        report["backend"] = backend_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["backend"]["ok"].as_bool().unwrap(),
        );
    }

    if !has_project && !args.backend {
        let dir = absolute_path(Path::new("."))?;
        let project_status = project::validate_dir(dir.as_path())?;
        report["project"] = project_status;
        report["ok"] = Value::Bool(
            report["ok"].as_bool().unwrap() && report["project"]["ok"].as_bool().unwrap(),
        );
    }

    Ok(report)
}

fn check_backend() -> Value {
    let url = backend_url();
    let client = NakamaClient::new(NakamaConfig::new(url));

    let mut result = match client {
        Ok(client) => match client.health() {
            Ok(_) => json!({
                "ok": true,
                "healthy": true,
                "message": "Nakama health check passed",
            }),
            Err(_) => json!({
                "ok": false,
                "healthy": false,
                "message": "Nakama health check failed",
            }),
        },
        Err(_) => json!({
            "ok": false,
            "healthy": false,
            "message": "Nakama configuration is invalid",
        }),
    };
    match lynx_world::schema::inspect_env() {
        Ok(status) => {
            result["ok"] = json!(result["ok"].as_bool().unwrap_or(false) && status.compatible());
            result["schema"] = json!(status);
        }
        Err(message) => {
            result["ok"] = json!(false);
            result["schema"] = json!({"error": message});
        }
    }
    result
}

fn backend_url() -> String {
    std::env::var("LYNX_NAKAMA_URL")
        .or_else(|_| std::env::var("NAKAMA_URL"))
        .or_else(|_| std::env::var("NAKAMA_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:7350".to_string())
}
