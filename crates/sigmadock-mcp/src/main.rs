//! Local stdio MCP bridge, optionally limited to one project's worker tools.
use anyhow::{Context, Result, bail};
use clap::Parser;
use serde_json::{Value, json};
use sigmadock_core::{Client, read_frame, socket_path};
use std::{
    io::{self, BufReader, Write},
    path::PathBuf,
};
#[derive(Parser)]
#[command(about = "Local SigmaDock MCP tools; autonomous spawn is opt-in")]
struct Args {
    #[arg(long)]
    allow_spawn: bool,
    #[arg(long)]
    project_id: Option<String>,
    #[arg(long, env = "SIGMA_DOCK_SOCKET", default_value_os_t = socket_path())]
    socket: PathBuf,
}
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})
}
fn tools(allow_spawn: bool, scoped: bool) -> Value {
    let worker = json!({"worker_id":{"type":"string"}});
    let mut tools = vec![
        tool(
            "list_workers",
            "List active workers within the project scope",
            json!({}),
            &[],
        ),
        tool(
            "get_worker_status",
            "Read one worker's derived status (working, needs_you, in_review, ready_to_merge); column is a deprecated alias",
            worker.clone(),
            &["worker_id"],
        ),
        tool(
            "message_worker",
            "Send a task instruction to a worker",
            json!({"worker_id":{"type":"string"},"message":{"type":"string"}}),
            &["worker_id", "message"],
        ),
        tool(
            "archive_worker",
            "Archive a stopped worker; preserves worktree and branch",
            worker,
            &["worker_id"],
        ),
    ];
    tools.push(tool(
        "list_queue",
        "List waiting task metadata within the project scope, up to 100 per page; use offset for subsequent pages",
        json!({"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100}}),
        &[],
    ));
    tools.push(tool(
        "cancel_queued",
        "Cancel a waiting task; preserves any files from an interrupted startup",
        json!({"id":{"type":"string"}}),
        &["id"],
    ));
    let mut project = json!({});
    if !scoped {
        project["project_id"] = json!({"type":"string"});
    }
    let mut read_required = Vec::new();
    if !scoped {
        read_required.push("project_id");
    }
    tools.push(tool(
        "read_planning_notes",
        "Read persistent planning notes and their revision",
        project.clone(),
        &read_required,
    ));
    let mut notes = project.clone();
    notes["text"] = json!({"type":"string"});
    notes["expected_revision"] = json!({"type":"integer","minimum":0});
    let mut write_required = read_required.clone();
    write_required.extend(["text", "expected_revision"]);
    tools.push(tool(
        "write_planning_notes",
        "Replace planning notes only if the expected revision still matches",
        notes,
        &write_required,
    ));
    if allow_spawn {
        let mut spawn = project;
        spawn["title"] = json!({"type":"string"});
        spawn["agent"] = json!({"type":"string"});
        spawn["prompt"] = json!({"type":"string"});
        spawn["queue"] = json!({"type":"boolean"});
        let mut required = read_required;
        required.push("title");
        tools.push(tool(
            "spawn_worker",
            "Create an isolated worker; queue: true waits durably for capacity",
            spawn,
            &required,
        ));
    }
    json!({"tools":tools})
}
fn validate(schema: &Value, arguments: &Value) -> Result<()> {
    let arguments = arguments
        .as_object()
        .context("tool arguments must be an object")?;
    let properties = schema["properties"]
        .as_object()
        .context("invalid tool schema")?;
    for (key, value) in arguments {
        let spec = properties
            .get(key)
            .with_context(|| format!("unexpected tool argument {key}"))?;
        let valid = match spec["type"].as_str() {
            Some("string") => value.is_string(),
            Some("integer") => value.as_u64().is_some(),
            Some("boolean") => value.is_boolean(),
            _ => false,
        };
        if !valid {
            bail!("invalid tool argument {key}");
        }
    }
    for key in schema["required"]
        .as_array()
        .context("invalid required fields")?
    {
        if !arguments.contains_key(key.as_str().unwrap()) {
            bail!("missing tool argument {key}");
        }
    }
    Ok(())
}
fn dispatch(client: &Client, request: &Value, args: &Args) -> Result<Value> {
    match request["method"].as_str().unwrap_or("") {
        "initialize" => Ok(
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"SigmaDock","version":"0.1.0"}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools(args.allow_spawn, args.project_id.is_some())),
        "tools/call" => {
            let name = request["params"]["name"]
                .as_str()
                .context("missing tool name")?;
            let available = tools(args.allow_spawn, args.project_id.is_some());
            let schema = available["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .context("tool is not enabled; spawn requires --allow-spawn at server startup")?;
            let result = (|| -> Result<Value> {
                let mut arguments = request["params"]["arguments"].clone();
                if arguments.is_null() {
                    arguments = json!({});
                }
                validate(&schema["inputSchema"], &arguments)?;
                client.check_version()?;
                if let Some(project) = &args.project_id {
                    if let Some(worker) = arguments["worker_id"].as_str() {
                        let status =
                            client.call("get_worker_status", json!({"worker_id":worker}))?;
                        if status["worker"]["project_id"].as_str() != Some(project) {
                            bail!("worker is outside this MCP project's scope");
                        }
                    }
                    arguments["project_id"] = json!(project);
                }
                if name == "archive_worker" {
                    arguments["cleanup"] = json!(false);
                }
                client.call(name, arguments)
            })();
            match result {
                Ok(value) => {
                    Ok(json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}]}))
                }
                Err(error) => {
                    Ok(json!({"content":[{"type":"text","text":error.to_string()}],"isError":true}))
                }
            }
        }
        _ => bail!("unknown MCP method"),
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    let client = Client {
        socket: args.socket.clone(),
    };
    let mut input = BufReader::new(io::stdin());
    let mut output = io::stdout();
    while let Some(frame) = read_frame(&mut input)? {
        let request: Value = match serde_json::from_slice(&frame) {
            Ok(value) => value,
            Err(_) => {
                serde_json::to_writer(
                    &mut output,
                    &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"parse error"}}),
                )?;
                output.write_all(b"\n")?;
                output.flush()?;
                continue;
            }
        };
        if request.get("id").is_none() {
            continue;
        }
        let id = &request["id"];
        let result = if request["jsonrpc"] == "2.0" {
            dispatch(&client, &request, &args)
        } else {
            Err(anyhow::anyhow!("invalid JSON-RPC version"))
        };
        let response = match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":error.to_string()}})
            }
        };
        serde_json::to_writer(&mut output, &response)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queuing_is_explicit_and_spawn_remains_opt_in() {
        let disabled = tools(false, false);
        assert!(
            !disabled["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "spawn_worker")
        );
        let enabled = tools(true, false);
        let schema = &enabled["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "spawn_worker")
            .unwrap()["inputSchema"];
        assert!(
            validate(
                schema,
                &json!({"project_id":"p","title":"task","queue":true})
            )
            .is_ok()
        );
        assert!(
            validate(
                schema,
                &json!({"project_id":"p","title":"task","queue":"true"})
            )
            .is_err()
        );
    }
    #[test]
    fn scoped_schemas_cannot_override_projects_or_cleanup() {
        let tools = tools(true, true);
        for tool in tools["tools"].as_array().unwrap() {
            assert!(
                tool["inputSchema"]["properties"]
                    .get("project_id")
                    .is_none()
            );
            assert!(tool["inputSchema"]["properties"].get("cleanup").is_none());
        }
        let archive = &tools["tools"][3]["inputSchema"];
        assert!(validate(archive, &json!({"worker_id":"w","cleanup":true})).is_err());
        assert!(validate(archive, &json!("malformed")).is_err());
    }
}
