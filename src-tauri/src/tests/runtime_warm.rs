use std::{process::Stdio, sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::{
    process::{Child, Command},
    sync::Mutex as AsyncMutex,
};
use uuid::Uuid;

use super::{reusable_runtime, WarmProvider, WarmRegistry, WarmRuntime, WarmState};
use crate::lifecycle_commands::start_agent_in_pool;
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

struct FakeState {
    alive: bool,
    busy: bool,
}

impl WarmState for FakeState {
    fn alive(&self) -> bool {
        self.alive
    }

    fn set_alive(&mut self, alive: bool) {
        self.alive = alive;
    }

    fn touch(&mut self) {}

    fn active_run_id(&self) -> Option<Uuid> {
        None
    }

    fn is_busy(&self) -> bool {
        self.busy
    }
}

struct FakeProvider;

impl WarmProvider for FakeProvider {
    type State = FakeState;
    const RUNTIME: &'static str = "fake";
    const PROCESS_TITLE: &'static str = "Fake warm process";
    const PROCESS_NAME: &'static str = "fake process";

    fn session_thread_id(&self) -> Option<&str> {
        None
    }

    async fn finish_turn_after_exit(
        _pool: &SqlitePool,
        _agent_id: Uuid,
        _runtime: &Arc<WarmRuntime<Self>>,
        _detail: String,
    ) {
    }
}

/// A live process in its own group, registered as the agent's warm runtime.
async fn register_process(
    registry: &WarmRegistry<FakeProvider>,
    agent_id: Uuid,
    busy: bool,
) -> Result<(Arc<WarmRuntime<FakeProvider>>, Child), String> {
    let mut child = Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| err.to_string())?;
    let stdin = child.stdin.take().ok_or("sleep stdin unavailable")?;
    let runtime = Arc::new(WarmRuntime {
        stdin: AsyncMutex::new(stdin),
        state: AsyncMutex::new(FakeState { alive: true, busy }),
        pid: child.id().map(|pid| pid as i32),
        environment_variables: String::new(),
        provider: FakeProvider,
    });
    registry.insert(agent_id, runtime.clone()).await;
    Ok((runtime, child))
}

async fn exits_soon(child: &mut Child) -> bool {
    tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .is_ok()
}

async fn restart_requested(pool: &SqlitePool, agent_id: Uuid) -> Result<bool, String> {
    let requested_at: Option<String> =
        sqlx::query_scalar("select runtime_restart_requested_at from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(|err| err.to_string())?;
    Ok(requested_at.is_some())
}

async fn reuse(
    pool: &SqlitePool,
    registry: &WarmRegistry<FakeProvider>,
    agent_id: Uuid,
) -> Result<Option<Arc<WarmRuntime<FakeProvider>>>, String> {
    reusable_runtime(pool, registry, agent_id, "", false).await
}

#[cfg(unix)]
#[tokio::test]
async fn restart_replaces_an_idle_process_once() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "restart-idle").await?;
        let registry = WarmRegistry::<FakeProvider>::default();
        let (_old, mut old_child) = register_process(&registry, agent_id, false).await?;

        start_agent_in_pool(&pool, agent_id).await?;
        assert!(restart_requested(&pool, agent_id).await?);

        assert!(reuse(&pool, &registry, agent_id).await?.is_none());
        assert!(exits_soon(&mut old_child).await, "the old process stops");
        assert!(registry.get(agent_id).await.is_none());
        assert!(!restart_requested(&pool, agent_id).await?);

        // The new process serves the request; later starts reuse it.
        let (new, _new_child) = register_process(&registry, agent_id, false).await?;
        let reused = reuse(&pool, &registry, agent_id).await?;
        assert!(reused.is_some_and(|runtime| Arc::ptr_eq(&runtime, &new)));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn restart_waits_until_the_process_finishes_its_turn() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "restart-busy").await?;
        let registry = WarmRegistry::<FakeProvider>::default();
        let (busy, mut child) = register_process(&registry, agent_id, true).await?;

        start_agent_in_pool(&pool, agent_id).await?;

        let reused = reuse(&pool, &registry, agent_id).await?;
        assert!(reused.is_some_and(|runtime| Arc::ptr_eq(&runtime, &busy)));
        assert!(child.try_wait().map_err(|err| err.to_string())?.is_none());
        assert!(restart_requested(&pool, agent_id).await?);

        busy.state.lock().await.busy = false;
        assert!(reuse(&pool, &registry, agent_id).await?.is_none());
        assert!(
            exits_soon(&mut child).await,
            "the process stops after its turn"
        );
        assert!(!restart_requested(&pool, agent_id).await?);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[tokio::test]
async fn restart_without_a_process_is_served_by_the_next_spawn() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "restart-cold").await?;
        let registry = WarmRegistry::<FakeProvider>::default();

        start_agent_in_pool(&pool, agent_id).await?;
        assert!(restart_requested(&pool, agent_id).await?);

        assert!(reuse(&pool, &registry, agent_id).await?.is_none());
        assert!(!restart_requested(&pool, agent_id).await?);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_idle_processes_leaves_a_process_in_a_turn_running() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let registry = WarmRegistry::<FakeProvider>::default();
        assert!(!registry.any_busy().await);
        let idle_agent = insert_test_agent(&pool, "update-idle").await?;
        let busy_agent = insert_test_agent(&pool, "update-busy").await?;
        let (idle, mut idle_child) = register_process(&registry, idle_agent, false).await?;
        let (busy, mut busy_child) = register_process(&registry, busy_agent, true).await?;
        assert!(registry.any_busy().await);

        registry
            .stop_idle_processes(&pool, "stopped for a newer Lantor build")
            .await;
        assert!(exits_soon(&mut idle_child).await, "the idle process stops");
        assert!(!idle.state.lock().await.alive);
        assert!(busy.state.lock().await.alive);
        assert!(busy_child
            .try_wait()
            .map_err(|err| err.to_string())?
            .is_none());
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
