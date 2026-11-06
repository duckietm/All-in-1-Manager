//! All-in-1 Manager core: what lives on this computer. Settings, the configuration folder, the server layout,
//! MariaDB credentials, SSH key generation and the encrypted settings backup.

pub mod backup;
pub mod config_dir;
pub mod credentials;
pub mod history;
pub mod keys;
pub mod layout;
pub mod settings;

pub use credentials::DatabaseCredentials;
pub use layout::ServerLayout;
pub use settings::InstallerSettings;
