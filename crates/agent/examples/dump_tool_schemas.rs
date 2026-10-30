//! Lin built-in tool schema dump.
//!
//! This example serializes the same normalized built-in tool definitions used
//! to construct language model requests. Pass `--catalog` to include rollout
//! metadata; neither mode represents a thread's filtered, registered tool set.

use anyhow::Result;

fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => {
            let mut tools = agent::built_in_tools().collect::<Vec<_>>();
            tools.sort_by(|left, right| left.name.cmp(&right.name));
            serde_json::to_writer_pretty(std::io::stdout().lock(), &tools)?;
        }
        [argument] if argument == "--catalog" => {
            let mut catalog = agent::BUILT_IN_TOOL_CATALOG.iter().collect::<Vec<_>>();
            catalog.sort_by_key(|tool| tool.name);
            let catalog = catalog
                .into_iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "feature_flag": tool.feature_flag,
                        "allowed_in_restricted_mode": tool.allowed_in_restricted_mode(),
                        "schema": tool.schema(),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_writer_pretty(std::io::stdout().lock(), &catalog)?;
        }
        _ => anyhow::bail!("usage: dump_tool_schemas [--catalog]"),
    }
    println!();
    Ok(())
}
