use super::*;

fn config(runtime: &str) -> ProbeConfig {
    ProbeConfig {
        agent_id: Uuid::new_v4(),
        handle: "fixture".into(),
        runtime: runtime.into(),
        working_directory: "/synthetic/workspace".into(),
        environment_variables: String::new(),
        model: String::new(),
        reasoning_effort: String::new(),
    }
}

fn fake(mode: &str) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-u", "-c", r#"
import json,sys,time
mode=sys.argv[1]; polls=0; plugins_loaded=False
for line in sys.stdin:
 m=json.loads(line)
 if mode=='timeout': time.sleep(60)
 claude=m.get('type')=='control_request'
 method=m.get('request',{}).get('subtype') if claude else m.get('method')
 if method=='initialized': continue
 result={}
 if method=='mcp_status':
  polls+=1
  if mode=='error':
   print(json.dumps({'type':'control_response','response':{'request_id':m['request_id'],'subtype':'error','error':'fixture error'}}),flush=True);continue
  result={'mcpServers':[] if polls==1 else [{'name':'docs','status':'pending' if mode=='pending' or polls==2 else 'connected','tools':[{'name':'read','description':'Read a document','inputSchema':{'secret':'never forwarded'}}]}]}
  if mode=='failed': result={'mcpServers':[{'name':'docs','status':'failed','error':'fixture failure'}]}
 if method=='mcpServerStatus/list':
  if mode=='mcp_error':
   print(json.dumps({'id':m['id'],'error':{'message':'fixture error'}}),flush=True);continue
  result={'data':[{'name':'apps','runtimeStatus':None,'authStatus':'oAuth','toolsError':None,'tools':{'find':{'name':'find','description':'Find records','inputSchema':{'type':'object'}}}}], 'nextCursor':None}
 if method=='plugin/list':
  if mode=='plugin_error':
   print(json.dumps({'id':m['id'],'error':{'message':'fixture error'}}),flush=True);continue
  plugins_loaded=True
  result={'marketplaces':[{'name':'fixture','plugins':[{'name':'installed','installed':True,'enabled':True},{'name':'not installed','installed':False}]}]}
 if method=='skills/list': result={'data':[{'cwd':m['params']['cwds'][0],'skills':[{'name':'plugin-skill' if plugins_loaded else 'incomplete','description':'A skill','enabled':True}],'errors':[]}]}
 response={'type':'control_response','response':{'request_id':m['request_id'],'subtype':'success','response':result}} if claude else {'id':m['id'],'result':result}
 print(json.dumps(response),flush=True)
"#, mode]);
    command
}

#[tokio::test]
async fn codex_discovers_installed_plugins_before_skills_without_inventing_status() {
    let config = config("codex");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("ready"),
        Duration::from_secs(2),
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(result.mcp.state, "ready");
    assert_eq!(result.mcp.items[0].status, "unknown");
    assert_eq!(result.plugins.items.len(), 1);
    assert_eq!(result.plugins.items[0].name, "installed");
    assert_eq!(result.skills.items[0].name, "plugin-skill");
    let encoded = serde_json::to_value(&result).unwrap();
    assert_eq!(
        encoded["mcp"]["items"][0]["tools"][0],
        json!({"name":"find","description":"Find records"})
    );
}

#[tokio::test]
async fn claude_waits_through_empty_and_pending_and_preserves_history_on_error() {
    let config = config("claude");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("ready"),
        Duration::from_millis(300),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result.mcp.items[0].status, "connected");
    assert_eq!(result.mcp.items[0].tools[0].name, "read");
    result.plugins = Inventory::ready(vec![PluginSummary {
        name: "from history".into(),
        description: String::new(),
        builtin: false,
        enabled: true,
    }]);
    probe_command(
        &config,
        &mut result,
        fake("error"),
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result.mcp.state, "unavailable");
    assert_eq!(result.plugins.items[0].name, "from history");
}

#[tokio::test]
async fn pending_is_unconfirmed_and_missing_cli_or_timeout_are_unavailable() {
    let config = config("claude");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("pending"),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(result.mcp.state, "unconfirmed");
    result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        Command::new("/nonexistent/lantor-cli"),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(result.mcp.state, "unavailable");
    probe_command(
        &config,
        &mut result,
        fake("timeout"),
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .await;
    assert_eq!(result.mcp.state, "unavailable");
}

#[tokio::test]
async fn one_codex_section_error_does_not_hide_other_sections() {
    let config = config("codex");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("mcp_error"),
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result.mcp.state, "unavailable");
    assert_eq!(result.plugins.items[0].name, "installed");
    assert_eq!(result.skills.items[0].name, "plugin-skill");
}

#[tokio::test]
async fn claude_server_error_reaches_the_inventory() {
    let config = config("claude");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("failed"),
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result.mcp.state, "ready");
    assert_eq!(result.mcp.items[0].status, "failed");
    assert_eq!(
        result.mcp.items[0].tools_error.as_deref(),
        Some("fixture failure")
    );
}

#[tokio::test]
async fn skills_are_unconfirmed_when_plugin_listing_fails() {
    let config = config("codex");
    let mut result = AgentTools::unavailable(&config);
    probe_command(
        &config,
        &mut result,
        fake("plugin_error"),
        Duration::from_secs(1),
        Duration::from_secs(3),
    )
    .await;
    assert_eq!(result.plugins.state, "unavailable");
    assert_eq!(result.skills.state, "unconfirmed");
    assert_eq!(result.skills.items[0].name, "incomplete");
}

#[tokio::test]
async fn dropping_a_probe_kills_its_child_process() {
    let process = ProbeProcess::start(fake("timeout"), Duration::from_millis(100)).unwrap();
    let pid = process.pid.unwrap();
    drop(process);
    let status = std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    // The process may be a zombie until Tokio reaps it, but it cannot run.
    if status.success() {
        let stat = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&stat.stdout)
                .trim()
                .starts_with('Z')
                || stat.stdout.is_empty()
        );
    }
}

#[tokio::test]
async fn probe_cleanup_also_stops_descendants_in_its_group() {
    for drop_early in [false, true] {
        let mut command = Command::new("/usr/bin/python3");
        command.args(["-u","-c","import subprocess,json,sys; p=subprocess.Popen(['/bin/sleep','60']); print(json.dumps({'pid':p.pid}),flush=True); sys.stdin.read()"]);
        let mut process = ProbeProcess::start(command, Duration::from_secs(1)).unwrap();
        let line = timeout(Duration::from_secs(2), process.lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        let pid = value["pid"].as_u64().unwrap();
        if drop_early {
            drop(process);
        } else {
            process.close().await;
        }
        let stat = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&stat.stdout);
        assert!(
            status.trim().is_empty() || status.trim().starts_with('Z'),
            "descendant must not run: {status}"
        );
    }
}
