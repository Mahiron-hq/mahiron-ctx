//! Model Context Protocol server mode.
//!
//! The server is a second consumer of the same engine, never a second implementation of
//! it: every request is translated into the exact command line that would produce the
//! same result, so the two interfaces cannot diverge in capability.

mod protocol;
#[cfg(feature = "mcp-sse")]
mod sse;
mod stdio;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::{Destination, Settings, Verbosity};
use crate::delivery::NeverConfirm;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::report::{RunReport, RunStatus};
use crate::{OUTPUT_SCHEMA_VERSION, TOOL_VERSION};

use protocol::{
    tool_payload, tool_text, Request, Response, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND,
    PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};

/// Documents at or below this size travel inline; larger ones are written out and the
/// agent is given the path, because oversized messages fail in ways agents handle badly.
const INLINE_LIMIT_BYTES: u64 = 256 * 1024;

/// Address the SSE transport listens on when none was given.
pub const DEFAULT_BIND: &str = "127.0.0.1:7681";

/// Start the server on the requested transport and serve until the peer disconnects.
pub fn serve(settings: Settings, transport: Option<&str>, bind: Option<&str>) -> Result<RunStatus> {
    let handler = Handler::new(settings);
    match transport.unwrap_or("stdio") {
        "stdio" => stdio::serve(&handler),
        "sse" => serve_sse(&handler, bind.unwrap_or(DEFAULT_BIND)),
        other => Err(Error::config(format!(
            "unknown transport `{other}`; expected stdio or sse"
        ))),
    }?;
    Ok(RunStatus::Success)
}

#[cfg(feature = "mcp-sse")]
fn serve_sse(handler: &Handler, bind: &str) -> Result<()> {
    sse::serve(handler, bind)
}

#[cfg(not(feature = "mcp-sse"))]
fn serve_sse(_handler: &Handler, _bind: &str) -> Result<()> {
    Err(Error::config(
        "this build has no SSE transport; use --transport stdio",
    ))
}

/// Settings supplied at startup act as the defaults a request may then override.
pub struct Handler {
    defaults: Settings,
}

/// A packaging request from an agent.
///
/// Every field corresponds to a command-line option; the request is turned into those
/// options rather than into settings directly.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PackageRequest {
    #[serde(default)]
    sources: Vec<String>,
    format: Option<String>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    no_gitignore: bool,
    #[serde(default)]
    hidden: bool,
    max_size: Option<String>,
    #[serde(default)]
    remove_blank_lines: bool,
    #[serde(default)]
    trim_trailing_whitespace: bool,
    #[serde(default)]
    compress: Option<Vec<String>>,
    tree_style: Option<String>,
    #[serde(default)]
    no_tree: bool,
    #[serde(default)]
    no_preface: bool,
    header: Option<String>,
    footer: Option<String>,
    remote_ref: Option<String>,
    #[serde(default)]
    keep_remote_copy: bool,
    #[serde(default)]
    trust_remote_config: Vec<String>,
    token_encoding: Option<String>,
    /// Write the document to this path instead of returning it.
    output: Option<String>,
    /// Replace `output` if it already exists.
    ///
    /// Defaults to false. It used to be unconditional, which meant a single request could
    /// overwrite any file the user could write — and, since the SSE transport validated
    /// no origin, a web page in the user's own browser could send that request.
    #[serde(default)]
    force: bool,
    /// Package files whose names identify them as credentials.
    #[serde(default)]
    allow_secrets: bool,
}

impl PackageRequest {
    /// The exact invocation this request stands for.
    fn to_arguments(&self) -> Vec<String> {
        let mut arguments: Vec<String> = vec!["mhrn".into()];
        arguments.extend(self.sources.iter().cloned());
        push_value(&mut arguments, "--format", self.format.as_deref());
        for pattern in &self.include {
            arguments.push("--include".into());
            arguments.push(pattern.clone());
        }
        for pattern in &self.exclude {
            arguments.push("--exclude".into());
            arguments.push(pattern.clone());
        }
        push_flag(&mut arguments, "--no-gitignore", self.no_gitignore);
        push_flag(&mut arguments, "--hidden", self.hidden);
        push_flag(&mut arguments, "--allow-secrets", self.allow_secrets);
        push_value(&mut arguments, "--max-size", self.max_size.as_deref());
        push_flag(
            &mut arguments,
            "--remove-blank-lines",
            self.remove_blank_lines,
        );
        push_flag(
            &mut arguments,
            "--trim-trailing-whitespace",
            self.trim_trailing_whitespace,
        );
        if let Some(languages) = &self.compress {
            // Attached rather than separate, matching the command line's own form.
            arguments.push(format!("--compress={}", languages.join(",")));
        }
        push_value(&mut arguments, "--tree-style", self.tree_style.as_deref());
        push_flag(&mut arguments, "--no-tree", self.no_tree);
        push_flag(&mut arguments, "--no-preface", self.no_preface);
        push_value(&mut arguments, "--header", self.header.as_deref());
        push_value(&mut arguments, "--footer", self.footer.as_deref());
        push_value(&mut arguments, "--remote-ref", self.remote_ref.as_deref());
        push_flag(&mut arguments, "--keep-remote-copy", self.keep_remote_copy);
        for designation in &self.trust_remote_config {
            arguments.push("--trust-remote-config".into());
            arguments.push(designation.clone());
        }
        push_value(
            &mut arguments,
            "--token-encoding",
            self.token_encoding.as_deref(),
        );
        push_value(&mut arguments, "--output", self.output.as_deref());
        push_flag(&mut arguments, "--force", self.force);
        arguments
    }
}

fn push_flag(arguments: &mut Vec<String>, flag: &str, active: bool) {
    if active {
        arguments.push(flag.into());
    }
}

fn push_value(arguments: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        arguments.push(flag.into());
        arguments.push(value.into());
    }
}

impl Handler {
    pub fn new(mut defaults: Settings) -> Self {
        // Nothing an agent did not ask for should reach its transport or its clipboard.
        defaults.reporting.verbosity = Verbosity::Quiet;
        defaults.reporting.progress = false;
        Self { defaults }
    }

    /// Answer one request, or nothing at all where the peer sent a notification.
    pub fn handle(&self, request: Request) -> Option<Response> {
        let id = request.id.clone()?;
        if !request.jsonrpc.is_empty() && request.jsonrpc != protocol::JSONRPC_VERSION {
            return Some(Response::error(
                id,
                INVALID_REQUEST,
                format!(
                    "unsupported JSON-RPC version `{}`; this server speaks {}",
                    request.jsonrpc,
                    protocol::JSONRPC_VERSION
                ),
            ));
        }
        let response = match request.method.as_str() {
            "initialize" => Response::result(id, self.initialize(&request.params)),
            "ping" => Response::result(id, json!({})),
            "tools/list" => Response::result(id, self.tool_catalogue()),
            "tools/call" => match self.call_tool(&request.params) {
                Ok(result) => Response::result(id, result),
                Err(Error::Mcp(detail)) => Response::error(id, INVALID_PARAMS, detail),
                // A run that failed is a result the agent can act on, not a request the
                // server was unable to understand.
                Err(error) => Response::result(id, tool_text(error.to_string(), true)),
            },
            "shutdown" => Response::result(id, json!({})),
            other => Response::error(id, METHOD_NOT_FOUND, format!("unknown method `{other}`")),
        };
        Some(response)
    }

    /// Answer `initialize`, honouring the revision the client asked for where possible.
    ///
    /// The version used to be a constant echoed at every client regardless of what it
    /// sent, which is not negotiation and left the server advertising one fixed revision
    /// for ever.
    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let agreed = match requested {
            Some(version) if SUPPORTED_PROTOCOL_VERSIONS.contains(&version) => version,
            // Unknown or absent: answer with the newest this server speaks and let the
            // client decide whether it can proceed.
            _ => PROTOCOL_VERSION,
        };
        json!({
            "protocolVersion": agreed,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "mahiron-ctx", "version": TOOL_VERSION },
        })
    }

    fn tool_catalogue(&self) -> Value {
        json!({ "tools": [
            {
                "name": "package_codebase",
                "description": "Consolidate one or more local sources, or a single remote \
                                repository, into one document. Returns the document itself \
                                when it is small enough to travel inline, and otherwise the \
                                path of the file it was written to. A document too large \
                                to travel inline is written to a temporary file that is \
                                not cleaned up automatically.",
                "inputSchema": package_schema(),
            },
            {
                "name": "preview_package",
                "description": "Report which files a packaging run would include, how large \
                                the document would be and how many tokens it would \
                                cost, without producing it.",
                "inputSchema": package_schema(),
            },
        ]})
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Mcp("a tool call must name a tool".into()))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let request: PackageRequest = serde_json::from_value(arguments)
            .map_err(|e| Error::Mcp(format!("could not read the tool arguments: {e}")))?;

        match name {
            "package_codebase" => self.package(request, false),
            "preview_package" => self.package(request, true),
            other => Err(Error::Mcp(format!("unknown tool `{other}`"))),
        }
    }

    fn package(&self, request: PackageRequest, preview: bool) -> Result<Value> {
        let wants_file = request.output.is_some();
        let mut settings = self.settings_for(&request)?;
        settings.dry_run = preview;

        if preview {
            let outcome = Engine::new(&settings)
                .with_confirmation(&NeverConfirm)
                .run()?;
            return Ok(tool_payload(
                summarise(&outcome.report),
                report_payload(&outcome.report, None),
            ));
        }

        if wants_file {
            // An agent cannot answer a prompt, so an existing destination is refused
            // unless the request said to replace it.
            let outcome = Engine::new(&settings)
                .with_confirmation(&NeverConfirm)
                .run()?;
            let path = match &settings.destination {
                Destination::File(path) => path.to_string_lossy().into_owned(),
                _ => String::new(),
            };
            return Ok(tool_payload(
                format!("{}\n\nWritten to {path}.", summarise(&outcome.report)),
                report_payload(&outcome.report, Some(path)),
            ));
        }

        let outcome = Engine::new(&settings)
            .with_confirmation(&NeverConfirm)
            .retaining_document()
            .run()?;
        let document = outcome.document.unwrap_or_default();

        if document.len() as u64 <= INLINE_LIMIT_BYTES {
            let text = String::from_utf8(document)
                .map_err(|_| Error::Mcp("the document is not valid UTF-8".into()))?;
            return Ok(tool_payload(text, report_payload(&outcome.report, None)));
        }

        let path = spill(&document, settings.format.extension())?;
        Ok(tool_payload(
            format!(
                "{}\n\nThe document is {} bytes, too large to return inline; it was written to \
                 {path}.",
                summarise(&outcome.report),
                document.len()
            ),
            report_payload(&outcome.report, Some(path)),
        ))
    }

    /// Apply a request on top of the settings the server was started with.
    fn settings_for(&self, request: &PackageRequest) -> Result<Settings> {
        use clap::Parser;

        let mut settings = crate::cli::Cli::try_parse_from(request.to_arguments())
            .map_err(|e| Error::Mcp(e.to_string()))?
            .to_settings()?;

        if request.sources.is_empty() {
            settings.sources = self.defaults.sources.clone();
        }
        settings.reporting = self.defaults.reporting.clone();
        settings.failure_policy = self.defaults.failure_policy;
        Ok(settings)
    }
}

/// Write an oversized document where the agent can fetch it, and leave it there.
///
/// Created with `O_EXCL` under an unpredictable name and readable only by its owner. The
/// old name was `mahiron-ctx-<pid>-<nanos>` written with a plain `fs::write`, which
/// follows symlinks: on a shared machine anyone able to guess that name — and the pid and
/// a coarse timestamp are not a secret — could have a link waiting at it, and the user's
/// source code would be written through it.
///
/// The file is deliberately *not* removed when the run ends: the agent is being given the
/// path precisely so it can read it afterwards. That retention is stated in the tool
/// description so the agent can pass it on.
fn spill(document: &[u8], extension: &str) -> Result<String> {
    use std::io::Write as _;

    let suffix = format!(".{extension}");
    let mut builder = tempfile::Builder::new();
    builder.prefix("mahiron-ctx-").suffix(&suffix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o600));
    }
    let mut file = builder
        .tempfile()
        .map_err(|e| Error::io(std::env::temp_dir(), e))?;
    file.write_all(document)
        .map_err(|e| Error::io(file.path(), e))?;
    file.flush().map_err(|e| Error::io(file.path(), e))?;
    let (_, path) = file.keep().map_err(|e| Error::io(e.file.path(), e.error))?;
    Ok(path.to_string_lossy().into_owned())
}

fn summarise(report: &RunReport) -> String {
    let mut lines = vec![format!(
        "{} of {} files packaged, {} bytes, {} tokens ({}).",
        report.included,
        report.discovered,
        report.output.bytes,
        report.output.tokens,
        report.format.label()
    )];
    if !report.warnings.is_empty() {
        lines.push(format!("{} warnings were recorded.", report.warnings.len()));
    }
    lines.join(" ")
}

fn report_payload(report: &RunReport, path: Option<String>) -> Value {
    json!({
        "schemaVersion": OUTPUT_SCHEMA_VERSION,
        "discovered": report.discovered,
        "included": report.included,
        "excluded": report.excluded,
        "bytes": report.output.bytes,
        "lines": report.output.lines,
        "tokens": report.output.tokens,
        "tokenEncoding": report.output.token_encoding.clone(),
        "format": report.format.label(),
        "warnings": report.warnings.iter().map(|w| json!({
            "path": w.path.clone(),
            "detail": w.kind.summary(),
        })).collect::<Vec<_>>(),
        "documentPath": path,
    })
}

fn package_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "sources": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Paths to package, or a single remote repository URL.",
            },
            "format": { "type": "string", "enum": ["markdown", "text", "xml", "json"] },
            "include": { "type": "array", "items": { "type": "string" } },
            "exclude": { "type": "array", "items": { "type": "string" } },
            "noGitignore": { "type": "boolean" },
            "hidden": { "type": "boolean" },
            "maxSize": { "type": "string", "description": "For example 500K or 2MB." },
            "removeBlankLines": { "type": "boolean" },
            "trimTrailingWhitespace": { "type": "boolean" },
            "compress": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Languages to reduce to structural signatures; empty for all supported.",
            },
            "treeStyle": { "type": "string", "enum": ["ascii", "compact"] },
            "noTree": { "type": "boolean" },
            "noPreface": { "type": "boolean" },
            "header": { "type": "string" },
            "footer": { "type": "string" },
            "remoteRef": { "type": "string" },
            "keepRemoteCopy": { "type": "boolean" },
            "trustRemoteConfig": { "type": "array", "items": { "type": "string" } },
            "tokenEncoding": { "type": "string", "enum": ["cl100k_base", "o200k_base"] },
            "output": {
                "type": "string",
                "description": "Write the document to this path instead of returning it.",
            },
            "force": {
                "type": "boolean",
                "description": "Replace the file named by `output` if it already exists.",
            },
            "allowSecrets": {
                "type": "boolean",
                "description": "Package files whose names identify them as credentials.",
            },
        },
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_becomes_the_invocation_it_stands_for() {
        let request = PackageRequest {
            sources: vec!["./src".into()],
            format: Some("json".into()),
            exclude: vec!["*.lock".into()],
            compress: Some(vec!["rust".into()]),
            ..Default::default()
        };
        let arguments = request.to_arguments();
        assert!(arguments.windows(2).any(|w| w == ["--format", "json"]));
        assert!(arguments.windows(2).any(|w| w == ["--exclude", "*.lock"]));
        assert!(arguments.iter().any(|a| a == "--compress=rust"));
    }

    #[test]
    fn a_request_does_not_overwrite_anything_unless_it_says_so() {
        let request = PackageRequest {
            output: Some("/etc/important".into()),
            ..Default::default()
        };
        assert!(
            !request.to_arguments().iter().any(|a| a == "--force"),
            "a request that did not ask to replace anything still forced the write"
        );

        let forcing = PackageRequest {
            output: Some("/tmp/x.md".into()),
            force: true,
            ..Default::default()
        };
        assert!(forcing.to_arguments().iter().any(|a| a == "--force"));
    }

    #[test]
    fn the_protocol_version_is_negotiated_rather_than_asserted() {
        let handler = Handler::new(Settings::default());
        let agreed = handler.initialize(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(agreed["protocolVersion"], "2024-11-05");

        // Something this server does not speak falls back to what it does speak.
        let fallback = handler.initialize(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(fallback["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn a_foreign_jsonrpc_version_is_refused() {
        let handler = Handler::new(Settings::default());
        let request: Request = serde_json::from_value(json!({
            "jsonrpc": "1.0", "id": 1, "method": "ping"
        }))
        .unwrap();
        let response = handler.handle(request).unwrap();
        assert_eq!(response.error.unwrap().code, INVALID_REQUEST);
    }

    #[test]
    fn unknown_arguments_are_rejected_rather_than_ignored() {
        let value = json!({ "sources": ["."], "unsupported": true });
        assert!(serde_json::from_value::<PackageRequest>(value).is_err());
    }

    #[test]
    fn unknown_methods_are_answered_with_an_error_not_a_result() {
        let handler = Handler::new(Settings::default());
        let request: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": 1, "method": "nonexistent"
        }))
        .unwrap();
        let response = handler.handle(request).unwrap();
        assert_eq!(response.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[test]
    fn notifications_are_answered_with_silence() {
        let handler = Handler::new(Settings::default());
        let request: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }))
        .unwrap();
        assert!(handler.handle(request).is_none());
    }
}
