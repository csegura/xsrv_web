// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::dispatch::Request;
use super::error::XError;
use super::screen::RESOURCE_ID_SHIFT;
use super::state::{ClientId, State};

/// What happens to the resources of a client when its connection ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseDown {
    Destroy,
    RetainPermanent,
    RetainTemporary,
}

impl State {
    pub(super) fn set_close_down_mode(&mut self, req: &Request) -> Result<(), XError> {
        req.reader().end()?;
        let mode = match req.detail {
            0 => CloseDown::Destroy,
            1 => CloseDown::RetainPermanent,
            2 => CloseDown::RetainTemporary,
            other => return Err(XError::Value(u32::from(other))),
        };
        self.set_client_close_down(req.client, mode);
        Ok(())
    }

    /// KillClient with a resource of a client that left and kept its resources, or with
    /// AllTemporary (0). A client that is still connected is not closed: that returns BadValue.
    pub(super) fn kill_client(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let resource = r.u32()?;
        r.end()?;
        if resource == 0 {
            let temporary: Vec<ClientId> = self
                .retained
                .iter()
                .filter(|(_, mode)| **mode == CloseDown::RetainTemporary)
                .map(|(client, _)| *client)
                .collect();
            for client in temporary {
                self.release_retained(client);
            }
            return Ok(());
        }
        let owner = resource >> RESOURCE_ID_SHIFT;
        if !self.retained.contains_key(&owner) || self.owners.get(&resource) != Some(&owner) {
            return Err(XError::Value(resource));
        }
        self.release_retained(owner);
        Ok(())
    }

    /// Keeps the resources of a client that closed down with a Retain mode. Returns false when
    /// they must be destroyed.
    pub(super) fn retain_resources(&mut self, client: ClientId) -> bool {
        let mode = self.client_close_down(client);
        if mode == CloseDown::Destroy {
            return false;
        }
        self.retained.insert(client, mode);
        true
    }

    fn release_retained(&mut self, client: ClientId) {
        self.retained.remove(&client);
        self.release_owned_resources(client);
        self.forget_orphan_owner(client);
        self.client_pixels.remove(&client);
        self.after_request();
        self.flush_display();
    }

    pub(super) fn release_owned_resources(&mut self, client: ClientId) {
        let owned: Vec<u32> = self
            .owners
            .iter()
            .filter(|(_, owner)| **owner == client)
            .map(|(id, _)| *id)
            .collect();
        for id in owned {
            if self.windows.contains_key(&id) {
                self.destroy_window(id);
            }
            self.forget_render_resource(id);
            self.release_pixmap(id, Some(client));
            self.gcs.remove(&id);
            self.clips.remove(&id);
            self.clip_masks.remove(&id);
            self.fonts.remove(&id);
            self.cursors.remove(&id);
            self.forget_region(id);
            self.owners.remove(&id);
        }
    }
}
