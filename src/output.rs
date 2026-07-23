use crate::error::{CarryCtxError, ExitCode};
use serde::Serialize;
use serde_json::Value;

/// Schema version for all JSON output
pub const SCHEMA_VERSION: u64 = 1;

/// Success envelope
#[derive(Debug, Serialize)]
pub struct SuccessEnvelope<T: Serialize> {
    pub schema_version: u64,
    pub command: String,
    pub success: bool,
    pub data: T,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub meta: EnvelopeMeta,
}

/// Error envelope
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub schema_version: u64,
    pub command: String,
    pub success: bool,
    pub error: ErrorPayload,
}

#[derive(Debug, Serialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub details: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Envelope metadata
#[derive(Debug, Serialize)]
pub struct EnvelopeMeta {
    pub timestamp: String,
}

/// Build a success envelope
pub fn success_envelope<T: Serialize>(
    command: &str,
    data: T,
    warnings: Vec<String>,
) -> SuccessEnvelope<T> {
    SuccessEnvelope {
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        success: true,
        data,
        warnings,
        meta: EnvelopeMeta {
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
    }
}

/// Build an error envelope from a CarryCtxError
pub fn error_envelope(command: &str, err: &CarryCtxError) -> ErrorEnvelope {
    ErrorEnvelope {
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        success: false,
        error: ErrorPayload {
            code: err.code.to_string(),
            message: err.message.clone(),
            details: err.details.clone(),
            suggestions: err.suggestions.clone(),
        },
    }
}

/// Output sink (stdout or stderr)
#[derive(Debug, Clone, Copy)]
pub enum OutputSink {
    Stdout,
    Stderr,
}

/// Render result to the appropriate stream
pub fn render_json<T: Serialize>(
    command: &str,
    result: Result<T, &CarryCtxError>,
    is_json: bool,
) -> (String, OutputSink, ExitCode) {
    match result {
        Ok(data) => {
            if is_json {
                let envelope = success_envelope(command, data, vec![]);
                let json = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                    r#"{"schemaVersion":1,"command":"error","success":false,"error":{"code":"INTERNAL_ERROR","message":"Failed to serialize response"}}"#.into()
                });
                (json, OutputSink::Stdout, ExitCode::Success)
            } else {
                // Text output - simple implementation
                let text = serde_json::to_string_pretty(&data).unwrap_or_default();
                (text, OutputSink::Stdout, ExitCode::Success)
            }
        }
        Err(err) => {
            let envelope = error_envelope(command, err);
            let json = serde_json::to_string(&envelope).unwrap_or_else(|_| {
                r#"{"schemaVersion":1,"command":"error","success":false,"error":{"code":"INTERNAL_ERROR","message":"Failed to serialize error"}}"#.into()
            });
            (json, OutputSink::Stderr, err.exit_code)
        }
    }
}
