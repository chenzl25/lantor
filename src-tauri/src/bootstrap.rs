use std::{
    env,
    net::{IpAddr, SocketAddr},
};

use sqlx::SqlitePool;

use crate::{
    activity_store::{load_agent_activities, load_agent_runs, load_agent_work_items},
    agent_profile::{load_agents, load_owner_profile},
    app::CommandResult,
    channels::{load_channel_members, load_channels, load_thread_activities},
    domain::{reminders::load_reminders, schedules::load_agent_schedules},
    launch_agent,
    message_store::{load_artifacts, load_messages, load_saved_messages},
    models::Bootstrap,
    owner_inbox::{load_dismissed_inbox_items, load_read_inbox_items},
    runtime::supervisor::load_supervisor_status,
    task_store::load_tasks,
    web,
};

fn configured_web_base_url() -> Option<String> {
    if let Ok(value) = env::var("LANTOR_WEB_PUBLIC_URL") {
        let trimmed = value.trim().trim_end_matches('/').to_owned();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let bind = web::resolve_web_bind()?;
    let addr = bind.parse::<SocketAddr>().ok()?;
    let host = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    Some(format!("http://{host}:{}", addr.port()))
}

pub(crate) async fn load_bootstrap(pool: &SqlitePool, db_url: String) -> CommandResult<Bootstrap> {
    let owner_profile = load_owner_profile(pool).await?;
    let channels = load_channels(pool).await?;
    let thread_activities = load_thread_activities(pool).await?;
    let channel_members = load_channel_members(pool).await?;
    let agents = load_agents(pool).await?;
    let messages = load_messages(pool).await?;
    let saved_messages = load_saved_messages(pool).await?;
    let dismissed_inbox_items = load_dismissed_inbox_items(pool).await?;
    let read_inbox_items = load_read_inbox_items(pool).await?;
    let artifacts = load_artifacts(pool).await?;
    let tasks = load_tasks(pool).await?;
    let reminders = load_reminders(pool).await?;
    let agent_schedules = load_agent_schedules(pool).await?;
    let agent_runs = load_agent_runs(pool).await?;
    let agent_work_items = load_agent_work_items(pool).await?;
    let agent_activities = load_agent_activities(pool).await?;
    let supervisor = load_supervisor_status(pool).await?;
    let launch_agent = launch_agent::load_launch_agent_status()?;

    Ok(Bootstrap {
        db_url,
        web_base_url: configured_web_base_url(),
        owner_profile,
        channels,
        thread_activities,
        channel_members,
        agents,
        messages,
        saved_messages,
        dismissed_inbox_items,
        read_inbox_items,
        artifacts,
        tasks,
        reminders,
        agent_schedules,
        agent_runs,
        agent_work_items,
        agent_activities,
        supervisor,
        launch_agent,
    })
}
