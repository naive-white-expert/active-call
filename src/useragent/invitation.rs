use std::sync::Arc;

use crate::{
    call::{RoutingState, sip::Invitation},
    config::InviteHandlerConfig,
    useragent::{playbook_handler::PlaybookInvitationHandler, webhook::WebhookInvitationHandler},
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use rsipstack::dialog::{
    DialogId,
    dialog::DialogStateReceiver,
    server_dialog::ServerInviteDialog,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub struct PendingDialog {
    pub token: CancellationToken,
    pub dialog: ServerInviteDialog,
    pub state_receiver: DialogStateReceiver,
}
pub struct PendingDialogGuard {
    pub id: DialogId,
    pub invitation: Invitation,
}

impl PendingDialogGuard {
    pub fn new(invitation: Invitation, id: DialogId, pending_dialog: PendingDialog) -> Self {
        invitation.add_pending(id.clone(), pending_dialog);
        info!(%id, "added pending dialog");
        Self { id, invitation }
    }

    /// Hangup first, then remove — mirrors DialogStateReceiverGuard ordering.
    pub async fn drop_async(&self) {
        let _ = self.invitation.get_pending_call(&self.id);
        if let Some(dialog) = self.invitation.dialog_layer.get_dialog(&self.id) {
            let id = dialog.id();
            dialog.hangup().await.ok();
            self.invitation.dialog_layer.remove_dialog(&id);
        }
    }
}

impl Drop for PendingDialogGuard {
    fn drop(&mut self) {
        let _ = self.invitation.get_pending_call(&self.id);
        if let Some(dialog) = self.invitation.dialog_layer.get_dialog(&self.id) {
            info!(%self.id, "hanging up pending dialog on drop");
            let layer = self.invitation.dialog_layer.clone();
            let id = dialog.id();
            crate::spawn(async move {
                dialog.hangup().await.ok();
                layer.remove_dialog(&id);
            });
        }
    }
}

#[async_trait]
pub trait InvitationHandler: Send + Sync {
    async fn on_invite(
        &self,
        _session_id: String,
        _cancel_token: CancellationToken,
        _dialog: ServerInviteDialog,
        _routing_state: Arc<RoutingState>,
    ) -> Result<()> {
        return Err(anyhow!("invite not handled"));
    }
}

pub fn default_create_invite_handler(
    config: Option<&InviteHandlerConfig>,
    app_state: Option<crate::app::AppState>,
) -> Option<Box<dyn InvitationHandler>> {
    match config {
        Some(InviteHandlerConfig::Webhook {
            url,
            urls,
            method,
            headers,
        }) => {
            let all_urls = if let Some(urls) = urls {
                urls.clone()
            } else if let Some(url) = url {
                vec![url.clone()]
            } else {
                vec![]
            };
            Some(Box::new(WebhookInvitationHandler::new(
                all_urls,
                method.clone(),
                headers.clone(),
            )))
        }
        Some(InviteHandlerConfig::Playbook { rules, default }) => {
            let app_state = match app_state {
                Some(s) => s,
                None => {
                    tracing::error!("app_state required for playbook invitation handler");
                    return None;
                }
            };
            let rules = rules.clone().unwrap_or_default();
            match PlaybookInvitationHandler::new(rules, default.clone(), app_state) {
                Ok(handler) => Some(Box::new(handler)),
                Err(e) => {
                    tracing::error!("failed to create playbook invitation handler: {}", e);
                    None
                }
            }
        }
        _ => None,
    }
}

pub type FnCreateInvitationHandler =
    fn(config: Option<&InviteHandlerConfig>) -> Result<Box<dyn InvitationHandler>>;
