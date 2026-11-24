//! Leads, from the app's side: a lead's draft, and where each machine keeps
//! its leads' folders (see [`crate::leads`]).

use gpui_kit::{App, Context, Window};

use super::RhoApp;
use crate::{
    leads::Leads,
    machines::{MachineId, Place},
    session_list::Kind,
    workspaces,
};

impl RhoApp {
    /// Shows a draft for a new lead and opens its session right away; see
    /// [`crate::draft::DraftView`].
    pub(super) fn new_lead(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.new_draft(Kind::Lead, window, cx);
    }

    /// `machine`'s leads root, as rho-ui guesses it before it is made, for
    /// the draft to show: this machine's in its rho home, a remote one's in
    /// `~/.rho`. Its session opens where [`Leads`] makes it.
    pub(super) fn leads_root(&self, machine: &MachineId, cx: &App) -> Place {
        let rho_home = if workspaces::reads_here(machine) {
            self.launch.rho_home.clone()
        } else {
            let home = self.workspaces.read(cx).home(machine);
            home.map(|home| home.join(".rho"))
        };
        let rho_home = rho_home.unwrap_or_else(|| "/".into());
        Place::new(machine.clone(), rho_home.join("leads"))
    }

    /// How to make folders in `machine`'s leads root.
    pub(super) fn leads(&self, machine: &MachineId, cx: &App) -> Leads {
        let reader = self.workspaces.read(cx).reader(machine);
        let executor = cx.background_executor().clone();
        Leads::new(reader, self.launch.rho_home.clone(), executor)
    }
}
