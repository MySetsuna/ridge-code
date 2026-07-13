use agent::{build_agent, default_tool, scripted, AgentState};
use langgraph::{MemoryCheckpointer, RunConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = build_agent(scripted(), default_tool())?;

    // 挂 checkpointer:每个超步落一份快照(可回滚 / 时间旅行)。
    let cp = MemoryCheckpointer::new();
    let init = AgentState::new("make the test suite pass");
    let out = app
        .invoke_with(init, &RunConfig::default(), Some(&cp), None)
        .await?;

    println!("== agent trace ==");
    for m in &out.messages {
        println!("  {m}");
    }

    println!("\n== supersteps (checkpoints) ==");
    for c in cp.history() {
        println!("  step {:>2} -> next {:?}", c.step, c.frontier);
    }

    println!(
        "\n== result: approved={} steps={} ==",
        out.approved, out.steps
    );
    Ok(())
}
