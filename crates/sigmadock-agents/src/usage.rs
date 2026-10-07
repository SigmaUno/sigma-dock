//! Read-only provider usage adapters. No credential files, transcripts or inference calls.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sigmadock_core::{AgentUsage, UsageWindow, read_frame, task_text, unix_time};
use std::{
    io::{BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub fn unavailable(provider: &str, message: &str) -> AgentUsage {
    AgentUsage {
        provider: provider.into(),
        source: "unavailable".into(),
        recorded_at: unix_time(),
        plan: None,
        windows: Vec::new(),
        lifetime_tokens: None,
        context_input_tokens: None,
        context_output_tokens: None,
        warnings: vec![message.into()],
    }
}
fn window(
    value: &Value,
    name: String,
    percent: &str,
    duration: Option<u64>,
    reset: &str,
) -> Option<UsageWindow> {
    let used = value[percent].as_f64()?;
    if !used.is_finite() || !(0. ..=100.).contains(&used) {
        return None;
    }
    Some(UsageWindow {
        name: task_text(&name, 128),
        used_percent: used,
        duration_minutes: duration,
        resets_at: value[reset].as_u64(),
    })
}
pub fn claude_status(data: &Value) -> AgentUsage {
    let mut report = unavailable(
        "claude",
        "Plan name and absolute subscription allowance are not exposed by this status-line adapter. Quota windows are shared account usage, not per-worker totals.",
    );
    report.source = "Claude Code status line".into();
    for (key, name, minutes) in [
        ("five_hour", "5-hour window", 300),
        ("seven_day", "7-day window", 10080),
    ] {
        if let Some(value) = window(
            &data["rate_limits"][key],
            name.into(),
            "used_percentage",
            Some(minutes),
            "resets_at",
        ) {
            report.windows.push(value);
        }
    }
    report.context_input_tokens = data["context_window"]["total_input_tokens"].as_u64();
    report.context_output_tokens = data["context_window"]["total_output_tokens"].as_u64();
    if report.windows.is_empty() {
        report.warnings.push("Subscription quota data is not present yet or is unavailable for this authentication mode/CLI version.".into());
    }
    report
}
fn codex_limits(account: &Value, limits: &Value, usage: Option<&Value>) -> AgentUsage {
    let mut report = unavailable(
        "codex",
        "Absolute quota allowances and per-worker subscription attribution are not reported. Window percentages are shared account usage.",
    );
    report.source = "Codex app-server account API".into();
    report.plan = account["account"]["planType"]
        .as_str()
        .map(|plan| task_text(plan, 64));
    let buckets: Vec<_> = if let Some(buckets) = limits["rateLimitsByLimitId"]
        .as_object()
        .filter(|buckets| !buckets.is_empty())
    {
        buckets
            .iter()
            .map(|(name, value)| (name.as_str(), value))
            .collect()
    } else {
        vec![("codex", &limits["rateLimits"])]
    };
    for (name, bucket) in buckets.into_iter().take(16) {
        for key in ["primary", "secondary"] {
            if let Some(value) = window(
                &bucket[key],
                format!("{name} · {key}"),
                "usedPercent",
                bucket[key]["windowDurationMins"].as_u64(),
                "resetsAt",
            ) {
                report.windows.push(value);
            }
        }
    }
    report.lifetime_tokens = usage.and_then(|usage| usage["summary"]["lifetimeTokens"].as_u64());
    if report.windows.is_empty() {
        report
            .warnings
            .push("The provider returned no subscription quota windows.".into());
    }
    report
}
struct Server {
    child: Child,
    writer: std::process::ChildStdin,
    receiver: mpsc::Receiver<Result<Value>>,
    deadline: Instant,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value> {
        writeln!(
            self.writer,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )?;
        self.writer.flush()?;
        loop {
            let message = self
                .receiver
                .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
                .context(
                    "Codex usage request timed out; check CLI version, sign-in and connection",
                )??;
            if message.get("method").is_some() && message.get("id").is_some() {
                // Never perform login, token exchange, approvals or tool requests for a usage check.
                writeln!(
                    self.writer,
                    "{}",
                    json!({"id":message["id"],"error":{"code":-32601,"message":"Usage reader only supports read-only requests"}})
                )?;
                continue;
            }
            if message["id"].as_u64() != Some(id) {
                continue;
            }
            if !message["error"].is_null() {
                bail!(
                    "Codex {method} unavailable; check CLI version, authentication mode and connection"
                );
            }
            return message
                .get("result")
                .cloned()
                .context("Codex returned an invalid usage response");
        }
    }
}
pub fn codex(cwd: &Path) -> Result<AgentUsage> {
    codex_program("codex", cwd)
}
fn codex_program(program: &str, cwd: &Path) -> Result<AgentUsage> {
    let mut child = Command::new(program)
        .args([
            "app-server",
            "--listen",
            "stdio://",
            "-c",
            "otel.exporter=\"none\"",
            "-c",
            "otel.trace_exporter=\"none\"",
            "-c",
            "otel.metrics_exporter=\"none\"",
        ])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not launch Codex app-server; install a compatible Codex CLI")?;
    let writer = child.stdin.take().context("Codex stdin unavailable")?;
    let stdout = child.stdout.take().context("Codex stdout unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(16);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(bytes)) = read_frame(&mut reader) {
            if sender
                .send(serde_json::from_slice(&bytes).map_err(Into::into))
                .is_err()
            {
                return;
            }
        }
    });
    let mut server = Server {
        child,
        writer,
        receiver,
        deadline: Instant::now() + Duration::from_secs(15),
    };
    server.request(0,"initialize",json!({"clientInfo":{"name":"sigmadock_usage","title":"SigmaDock","version":env!("CARGO_PKG_VERSION")}}))?;
    writeln!(server.writer, "{}", json!({"method":"initialized"}))?;
    server.writer.flush()?;
    let account = server.request(1, "account/read", json!({"refreshToken":false}))?;
    if account["account"].is_null()
        || matches!(
            account["account"]["type"].as_str(),
            Some("apiKey" | "amazonBedrock")
        )
    {
        return Ok(unavailable(
            "codex",
            "No supported Codex subscription account is available. Sign in through Codex; API-key/Bedrock usage is not a ChatGPT subscription allowance.",
        ));
    }
    let limits = server.request(2, "account/rateLimits/read", Value::Null)?;
    let usage = server.request(3, "account/usage/read", Value::Null);
    let mut report = codex_limits(&account, &limits, usage.as_ref().ok());
    if usage.is_err() {
        report.warnings.push("Account token-activity totals are unavailable in this CLI/account; quota windows remain usable.".into());
    }
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn app_server_protocol_is_read_only_and_optional_usage_failure_keeps_limits() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "sigma-usage-{}-{}",
            std::process::id(),
            unix_time()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fake = root.join("codex");
        std::fs::write(&fake, r#"#!/usr/bin/env python3
import json,sys,pathlib
log=pathlib.Path('requests.jsonl')
for line in sys.stdin:
    request=json.loads(line)
    with log.open('a') as out: out.write(json.dumps(request)+'\n')
    method=request.get('method')
    if method=='initialize': result={}
    elif method=='initialized': continue
    elif method=='account/read':
        assert request['params']=={'refreshToken':False}
        result={'account':{'type':'chatgpt','planType':'pro','email':'secret@example.invalid'}}
    elif method=='account/rateLimits/read': result={'rateLimits':{'primary':{'usedPercent':42,'windowDurationMins':300,'resetsAt':1234}}}
    elif method=='account/usage/read':
        print(json.dumps({'id':request['id'],'error':{'code':-32601,'message':'unsupported'}}),flush=True)
        continue
    else: raise RuntimeError('Unexpected non-read-only method: '+str(method))
    print(json.dumps({'id':request['id'],'result':result}),flush=True)
"#).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let report = codex_program(fake.to_str().unwrap(), &root).unwrap();
        assert_eq!(report.windows[0].used_percent, 42.);
        assert_eq!(report.plan.as_deref(), Some("pro"));
        assert!(report.lifetime_tokens.is_none());
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("unavailable"))
        );
        assert!(!serde_json::to_string(&report).unwrap().contains("secret@"));
        let requests: Vec<Value> = std::fs::read_to_string(root.join("requests.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests
                .iter()
                .map(|request| request["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "initialize",
                "initialized",
                "account/read",
                "account/rateLimits/read",
                "account/usage/read"
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn missing_values_are_unknown_and_context_is_not_account_consumption() {
        let report = claude_status(
            &json!({"context_window":{"total_input_tokens":123},"rate_limits":{"five_hour":{"used_percentage":25,"resets_at":1234}},"email":"private","workspace":{"current_dir":"secret"}}),
        );
        assert_eq!(report.windows[0].used_percent, 25.);
        assert_eq!(report.context_input_tokens, Some(123));
        assert!(report.plan.is_none() && report.lifetime_tokens.is_none());
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(!serialized.contains("private") && !serialized.contains("secret"));
        assert!(claude_status(&json!({})).windows.is_empty());
        assert!(
            claude_status(&json!({"rate_limits":{"five_hour":{"used_percentage":101}}}))
                .windows
                .is_empty()
        );
    }
    #[test]
    fn codex_multibucket_and_legacy_limits_preserve_provider_percentages() {
        let account = json!({"account":{"type":"chatgpt","planType":"pro","email":"private"}});
        let limits = json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":1234}},"other":{"secondary":{"usedPercent":0,"windowDurationMins":10080}}}});
        let report = codex_limits(
            &account,
            &limits,
            Some(&json!({"summary":{"lifetimeTokens":1000}})),
        );
        assert_eq!(report.plan.as_deref(), Some("pro"));
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.lifetime_tokens, Some(1000));
        assert!(!serde_json::to_string(&report).unwrap().contains("private"));
        assert_eq!(
            codex_limits(
                &account,
                &json!({"rateLimits":{"primary":{"usedPercent":99}}}),
                None
            )
            .windows[0]
                .used_percent,
            99.
        );
    }
}
