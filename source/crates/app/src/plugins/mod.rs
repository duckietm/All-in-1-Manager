//! Plugins: the optional parts of the window. Each one owns some pages; switched off on the Plugins page, those pages
//! leave the sidebar, the Addons page and the dashboard. A plugin may also need a data pack (the client images, the
//! default catalogue) that is downloaded on demand instead of being built into the program: see [`packs`].
//! Which plugins are off is kept in settings.json (`DisabledPlugins`), so the window opens the way it was left.
//!
//! A new addon: its pages in `gui::Page`, its wizard or view, and an entry in [`PLUGINS`].

pub mod packs;

use std::collections::BTreeSet;
use std::sync::OnceLock;

use parking_lot::Mutex;

use crate::gui::Page;
use crate::i18n::n;
use packs::Pack;

/// One plugin: what the Plugins page shows, and the pages it owns.
pub struct Plugin {
    /// Kept in settings.json; never rename one.
    pub id: &'static str,
    /// English; translated with `t()`.
    pub title: &'static str,
    pub description: &'static str,
    /// Its pages. The first one is where the Plugins page's "Open" goes.
    pub pages: &'static [Page],
    /// The data it needs that is not in the program, if any.
    pub pack: Option<&'static Pack>,
}

/// Every plugin, in the order of the Plugins page.
pub static PLUGINS: &[Plugin] = &[
    Plugin {
        id: "cms",
        title: "CMS",
        description: n("AtomCMS, EpicNext or your own CMS as the hotel's website."),
        pages: &[Page::AtomCms, Page::EpicNext, Page::CustomCms, Page::RemoveCms],
        pack: None,
    },
    Plugin {
        id: "gitea",
        title: "Gitea",
        description: n("Your own Git server on the hotel's server."),
        pages: &[Page::Gitea, Page::RemoveGitea],
        pack: None,
    },
    Plugin {
        id: "fail2ban",
        title: "Fail2Ban",
        description: n("Bans addresses that guess SSH passwords."),
        pages: &[Page::Fail2Ban, Page::RemoveFail2Ban],
        pack: None,
    },
    Plugin {
        id: "crowdsec",
        title: "CrowdSec",
        description: n("Blocks attackers on every route through Traefik."),
        pages: &[Page::CrowdSec, Page::RemoveCrowdSec],
        pack: None,
    },
    Plugin {
        id: "default-catalog",
        title: n("Default catalogue"),
        description: n("A complete catalogue for a new hotel."),
        pages: &[Page::DefaultCatalog],
        pack: Some(&packs::CATALOG),
    },
    Plugin {
        id: "catalog-rip",
        title: n("Catalogue ripper"),
        description: n("Copies the catalogue of Habbo or a retro hotel through G-Earth, ready to import."),
        pages: &[Page::CatalogRip],
        pack: None,
    },
    Plugin {
        id: "sql-editor",
        title: "SQL Editor",
        description: n("Browse and edit the hotel's tables, run queries, export to CSV."),
        pages: &[Page::SqlEditor],
        pack: None,
    },
    Plugin {
        id: "habbo-assets",
        title: n("Habbo and retro assets"),
        description: n("Download Habbo's and Nitro retros' furniture and clothes, and the asset tools on this computer."),
        pages: &[Page::GetAssets, Page::RetroAssets, Page::AssetTools],
        pack: None,
    },
    Plugin {
        id: "deploy-assets",
        title: n("Deploy Assets"),
        description: n("The downloaded assets, the client images and the UI texts to the server."),
        pages: &[Page::DeployAssets],
        pack: Some(&packs::CLIENT_ASSETS),
    },
    Plugin {
        id: "asset-import",
        title: n("Asset import and fixes"),
        description: n("Import furniture into items_base and the catalogue, fix offer IDs and items_base."),
        pages: &[Page::ImportAssets, Page::ImportCustom, Page::OfferFix, Page::ItemFix],
        pack: None,
    },
    Plugin {
        id: "asset-explorer",
        title: "Asset Explorer",
        description: n("Two panes: this computer and the server's gamedata."),
        pages: &[Page::AssetExplorer],
        pack: None,
    },
];

/// The plugin a page belongs to; None for the pages that are always there.
pub fn plugin_for(page: Page) -> Option<&'static Plugin> {
    PLUGINS.iter().find(|p| p.pages.contains(&page))
}

fn disabled() -> &'static Mutex<BTreeSet<String>> {
    static STATE: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(aio_core::InstallerSettings::load().disabled_plugins.into_iter().collect()))
}

pub fn is_enabled(id: &str) -> bool {
    !disabled().lock().contains(id)
}

/// Switches a plugin on or off and saves the choice.
pub fn set_enabled(id: &str, enabled: bool) {
    let list: Vec<String> = {
        let mut set = disabled().lock();
        if enabled {
            set.remove(id);
        } else {
            set.insert(id.to_string());
        }
        set.iter().cloned().collect()
    };
    let mut settings = aio_core::InstallerSettings::load();
    settings.disabled_plugins = list;
    // A read-only configuration folder only means the choice is not remembered.
    let _ = settings.save();
}

/// Whether the page is in the menus: always for the core pages, else when its plugin is on.
pub fn page_visible(page: Page) -> bool {
    plugin_for(page).is_none_or(|p| is_enabled(p.id))
}

/// Whether the Addons page has anything to show.
pub fn any_addon_visible() -> bool {
    crate::views::addons::entries().iter().any(|e| page_visible(e.page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_pages_belong_to_one_plugin() {
        let mut ids = BTreeSet::new();
        let mut pages = Vec::new();
        for plugin in PLUGINS {
            assert!(ids.insert(plugin.id), "{} twice", plugin.id);
            assert!(!plugin.pages.is_empty(), "{} has no page", plugin.id);
            for page in plugin.pages {
                assert!(!pages.contains(page), "{page:?} in two plugins");
                pages.push(*page);
            }
        }
    }

    #[test]
    fn the_core_pages_are_not_plugins() {
        for page in [Page::Dashboard, Page::Install, Page::Addons, Page::Settings, Page::About, Page::Plugins, Page::Tools, Page::CatalogManager] {
            assert!(plugin_for(page).is_none(), "{page:?}");
            assert!(page_visible(page));
        }
    }

    #[test]
    fn every_addon_tile_is_a_plugin() {
        for entry in crate::views::addons::entries().iter().chain(crate::views::addons::asset_entries().iter()) {
            assert!(plugin_for(entry.page).is_some(), "{:?} has no plugin", entry.page);
        }
    }
}
