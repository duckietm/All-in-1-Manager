//! The pages and install steps (SourceCode/App/Professional/Views). Every install step module exports `step()`
//! (`clone` exports `steps()`); every addon module exports `wizard(shared)`, which builds its steps and finish page.

pub mod about;
pub mod addons;
pub mod asset_explorer;
pub mod asset_tools;
pub mod get_assets;
pub mod atom_cms;
pub mod clone;
pub mod crowdsec;
pub mod custom_cms;
pub mod dashboard;
pub mod default_catalog;
pub mod import_assets;
pub mod debug;
pub mod deploy;
pub mod dev_tools;
pub mod doctor;
pub mod emulator_monitor;
pub mod emulator;
pub mod epicnext;
pub mod fail2ban;
pub mod finished;
pub mod gitea;
pub mod harden;
pub mod history;
pub mod line_chart;
pub mod login;
pub mod mariadb;
pub mod nginx;
pub mod octane;
pub mod php;
pub mod remove_cms;
pub mod remove_crowdsec;
pub mod remove_fail2ban;
pub mod remove_gitea;
pub mod server_status;
pub mod settings;
pub mod sql_editor;
pub mod ssh_key;
pub mod ssh_login;
pub mod terminal;
pub mod tools;
pub mod update_dialog;
pub mod web_access;

use std::sync::Arc;

use crate::gui::step::InstallStep;
use crate::gui::wizard::Wizard;
use crate::gui::Shared;
use crate::i18n::t;

/// The full server installation, in order. A new step only needs adding here. (ServerWizard.ServerSteps)
pub fn server_steps() -> Vec<Arc<dyn InstallStep>> {
    let mut steps: Vec<Arc<dyn InstallStep>> = vec![ssh_key::step(), Arc::new(nginx::NginxStep), php::step(), mariadb::step(), dev_tools::step()];
    steps.extend(clone::steps());
    steps.extend([emulator::step(), web_access::step(), octane::step(), harden::step(), ssh_login::step()]);
    steps
}

pub(crate) fn wizards(shared: &Shared) -> crate::gui::Wizards {
    crate::gui::Wizards {
        install: Wizard::new(
            shared.runtime.clone(),
            &shared.ctx,
            &shared.dialogs,
            server_steps(),
            true,
            Arc::new(finished::server),
            t("Upload assets"),
            t("Completed"),
        ),
        atom: atom_cms::wizard(shared),
        custom_cms: custom_cms::wizard(shared),
        epicnext: epicnext::wizard(shared),
        remove_cms: remove_cms::wizard(shared),
        gitea: gitea::wizard(shared),
        remove_gitea: remove_gitea::wizard(shared),
        fail2ban: fail2ban::wizard(shared),
        remove_fail2ban: remove_fail2ban::wizard(shared),
        crowdsec: crowdsec::wizard(shared),
        remove_crowdsec: remove_crowdsec::wizard(shared),
        default_catalog: default_catalog::wizard(shared),
        deploy: deploy::wizard(shared),
        import_assets: import_assets::wizard(shared, import_assets::Mode::Habbo),
        import_custom: import_assets::wizard(shared, import_assets::Mode::Custom),
    }
}
