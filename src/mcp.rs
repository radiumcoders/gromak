//! `gromak mcp`: a Model Context Protocol server over stdio that lets AI
//! agents ("clankers") work on a workspace of their own without disturbing
//! the user.
//!
//! An MCP client (Claude Code, Cursor, …) starts `gromak mcp` and speaks
//! newline-delimited JSON-RPC 2.0 on its stdin and stdout. Each tool call is
//! forwarded to the running gromak over its pipe (see [`crate::agent`]).
//!
//! gromak handles the window-manager side: monitors, workspaces, launching
//! apps onto the agent's workspace, keeping them drivable while the user
//! looks elsewhere. Operating the apps themselves is left to Vercel's
//! agent-desktop (`agent-desktop mcp`), which reads and drives windows
//! through UI Automation without moving the pointer or taking focus.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-06-18";

const INSTRUCTIONS: &str = "\
gromak is the user's tiling window manager. It gives you a workspace of your own, so you can \
work in desktop apps without disturbing what the user is doing.

1. Call gromak_monitors. Monitors have names such as \"top\", \"bottom\", \"left\", \"right\" \
and \"primary\"; when the user names a monitor, use that name.
2. Call gromak_claim_workspace with that monitor. You get a workspace id like \"2:9\".
3. Start apps only with gromak_launch on your workspace. Never start them any other way \
(agent-desktop launch, a shell, Start menu): they would open on the user's screen.
4. Operate the windows with agent-desktop's MCP tools (desktop_snapshot, desktop_click, \
desktop_set_value, desktop_select, desktop_toggle, desktop_scroll, desktop_screenshot, ...), \
passing the window id (\"w-123456\") that gromak_launch or gromak_windows returned. Your windows \
stay off-screen while the user looks elsewhere, and agent-desktop drives them through UI \
Automation without the mouse or keyboard focus.
5. Stay out of the user's way: never use agent-desktop's --headed mode, focus-window, focus, \
type (use set-value instead), press (it types into the user's focused window), or any mouse-* \
command.
6. When the task is done, close your windows (gromak_close_window) and call \
gromak_release_workspace.";

fn tools() -> Value {
    let workspace = json!({
        "type": "string",
        "description": "Workspace id from gromak_claim_workspace, like \"2:9\" or \"bottom:9\"."
    });
    let window = json!({ "type": "string", "description": "Window id like \"w-123456\"." });
    json!([
        {
            "name": "gromak_monitors",
            "title": "List monitors",
            "description": "Lists the monitors with the names they answer to (\"top\", \"bottom\", \"left\", \
                \"right\", \"primary\", or their number), what each shows, and which one the user is on.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "gromak_claim_workspace",
            "title": "Claim a workspace",
            "description": "Claims a workspace for you on a monitor. Its windows stay off-screen while the user \
                looks at something else, but keep running and can be driven with agent-desktop. Returns the \
                workspace id to use with the other tools.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "monitor": {
                        "type": "string",
                        "description": "Monitor name: \"top\", \"bottom\", \"left\", \"right\", \"primary\" or a number. Default \"primary\"."
                    },
                    "workspace": {
                        "type": "integer", "minimum": 1, "maximum": 10,
                        "description": "Workspace number on that monitor. Default: the highest free one."
                    },
                    "agent": { "type": "string", "description": "Your name, shown in gromak's log." }
                }
            }
        },
        {
            "name": "gromak_launch",
            "title": "Launch an app on your workspace",
            "description": "Starts a program (exe name or path, a document, or a URI) on your workspace without \
                taking the user's focus, waits for its window, and returns the new windows with their ids \
                for agent-desktop.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspace": workspace,
                    "command": { "type": "string", "description": "e.g. \"notepad.exe\", \"C:\\\\Tools\\\\app.exe\", \"calc\"." },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Command-line arguments." },
                    "cwd": { "type": "string", "description": "Working directory." },
                    "timeout_ms": { "type": "integer", "description": "How long to wait for a window. Default 15000." }
                },
                "required": ["workspace", "command"]
            }
        },
        {
            "name": "gromak_windows",
            "title": "List your windows",
            "description": "Lists the windows on your workspace (or on all agent workspaces), with ids for agent-desktop.",
            "inputSchema": { "type": "object", "properties": { "workspace": workspace } },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "gromak_move_window",
            "title": "Move a window to your workspace",
            "description": "Moves a window gromak manages onto your workspace. Only do this when the user asked \
                you to take over that window.",
            "inputSchema": {
                "type": "object",
                "properties": { "window": window, "workspace": workspace },
                "required": ["window", "workspace"]
            }
        },
        {
            "name": "gromak_close_window",
            "title": "Close one of your windows",
            "description": "Asks a window on your workspace to close (the app may ask to save first).",
            "inputSchema": { "type": "object", "properties": { "window": window }, "required": ["window"] },
            "annotations": { "destructiveHint": true }
        },
        {
            "name": "gromak_release_workspace",
            "title": "Release your workspace",
            "description": "Hands the workspace back to the user. Windows still on it stay there.",
            "inputSchema": { "type": "object", "properties": { "workspace": workspace }, "required": ["workspace"] }
        }
    ])
}

/// The gromak request for a tool call.
fn to_request(tool: &str, args: &Value) -> Option<Value> {
    let op = match tool {
        "gromak_monitors" => "monitors",
        "gromak_claim_workspace" => "claim",
        "gromak_launch" => "launch",
        "gromak_windows" => "windows",
        "gromak_move_window" => "move_window",
        "gromak_close_window" => "close_window",
        "gromak_release_workspace" => "release",
        _ => return None,
    };
    let mut request = if args.is_object() { args.clone() } else { json!({}) };
    request["op"] = json!(op);
    Some(request)
}

fn call_tool(params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let Some(request) = to_request(name, &params["arguments"]) else {
        return tool_result(format!("unknown tool {name}"), true);
    };
    match crate::agent::request(&request) {
        Ok(reply) => {
            let failed = reply["ok"] != true;
            let text = if failed {
                reply["error"].as_str().unwrap_or("gromak refused the request").to_string()
            } else {
                serde_json::to_string_pretty(&reply).unwrap_or_default()
            };
            tool_result(text, failed)
        }
        Err(e) => tool_result(e, true),
    }
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The reply to one JSON-RPC message, or `None` for notifications.
fn handle(message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => {
            let version = message["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "gromak", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call_tool(&message["params"])),
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    })
}

/// Serves MCP on stdin/stdout until the client closes stdin.
pub fn run() -> i32 {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // Tolerate a byte-order mark some clients write before the first message.
        let line = line.trim_start_matches('\u{feff}');
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(message) => handle(&message),
            Err(e) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        };
        if let Some(reply) = reply {
            if writeln!(stdout, "{reply}").and_then(|_| stdout.flush()).is_err() {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_initialize_and_lists_tools() {
        let init = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}}}))
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "gromak");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let list = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let names: Vec<&str> =
            list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"gromak_launch") && names.contains(&"gromak_claim_workspace"));
        // Notifications get no reply; unknown methods get an error.
        assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        assert_eq!(handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})).unwrap()["error"]["code"], -32601);
    }

    #[test]
    fn maps_tools_to_requests() {
        let request = to_request("gromak_launch", &json!({"workspace": "2:9", "command": "notepad"})).unwrap();
        assert_eq!(request["op"], "launch");
        assert_eq!(request["command"], "notepad");
        assert!(to_request("rm_rf", &json!({})).is_none());
    }
}
