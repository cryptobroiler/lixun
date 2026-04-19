use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::CreateSessionOptions;
use futures::StreamExt;
use tokio::sync::mpsc;

pub async fn spawn_global_toggle_listener(preferred_trigger: String) -> anyhow::Result<mpsc::Receiver<()>> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(CreateSessionOptions::default()).await?;
    let request = portal
        .bind_shortcuts(
            &session,
            &[NewShortcut::new("toggle", "Toggle Lupa launcher")
                .preferred_trigger(Some(preferred_trigger.as_str()))],
            None,
            BindShortcutsOptions::default(),
        )
        .await?;
    let _ = request.response()?;

    let mut activated = portal.receive_activated().await?;
    let (tx, rx) = mpsc::channel(16);

    tokio::spawn(async move {
        while let Some(event) = activated.next().await {
            if event.shortcut_id() == "toggle" {
                let _ = tx.send(()).await;
            }
        }
    });

    Ok(rx)
}
