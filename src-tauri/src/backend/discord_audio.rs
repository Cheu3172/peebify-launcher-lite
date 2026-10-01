// ------------ Discord Audio Clients ------------
// Finds which Discord apps are running (stable, PTB, Canary, Development) so the overlay recorder can capture Discord's voice audio.
// Only the process lookup lives here; the recording itself is done elsewhere.
use serde_json::{json, Value};

pub const AUDIO_CLIENTS: &[(&str, &str)] = &[
    ("discord.exe", "Discord"),
    ("discordptb.exe", "Discord PTB"),
    ("discordcanary.exe", "Discord Canary"),
    ("discorddevelopment.exe", "Discord Development"),
];

pub fn running_audio_clients() -> Vec<(String, String, u32)> {
    let processes = super::process_utils::snapshot_with_parents();
    let mut out: Vec<(String, String, u32)> = Vec::new();

    for (executable, label) in AUDIO_CLIENTS {
        let mine: Vec<(u32, u32)> = processes
            .iter()
            .filter(|(name, _, _)| name == executable)
            .map(|(_, pid, parent)| (*pid, *parent))
            .collect();
        if mine.is_empty() {
            continue;
        }
        let owned: Vec<u32> = mine.iter().map(|(pid, _)| *pid).collect();
        let root = mine
            .iter()
            .find(|(_, parent)| !owned.contains(parent))
            .map(|(pid, _)| *pid)
            .unwrap_or(mine[0].0);
        out.push(((*executable).to_string(), (*label).to_string(), root));
    }
    out
}

pub(super) async fn list_audio_clients() -> Result<Value, String> {
    let clients: Vec<Value> = running_audio_clients()
        .into_iter()
        .map(|(id, label, pid)| json!({ "id": id, "label": label, "pid": pid }))
        .collect();
    Ok(super::ok_with(json!({ "clients": clients })))
}
