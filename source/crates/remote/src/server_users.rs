//! The accounts the emulator can run as (Settings → Server layout), and creating a new one. (ServerUsers.cs)

use std::collections::HashSet;

use aio_core::layout::ROOT_ACCOUNT;
use aio_core::ServerLayout;

use crate::script::fill;
use crate::server::{Result, Server, ServerError};

/// An account on the server, from /etc/passwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUser {
    pub name: String,
    pub uid: i32,
    pub home: String,
    pub shell: String,
    pub in_www_data: bool,
}

impl ServerUser {
    /// A service account (below uid 1000), such as the one the Emulator step creates.
    pub fn is_system(&self) -> bool {
        self.uid > 0 && self.uid < 1000
    }

    pub fn can_log_in(&self) -> bool {
        !self.shell.ends_with("nologin") && !self.shell.ends_with("/false")
    }
}

const LIST: &str = r#"echo "www=$(getent group www-data | cut -d: -f4)"
getent passwd | awk -F: '{ print "user=" $1 ":" $3 ":" $6 ":" $7 }'"#;

/// root, the people's accounts (uid 1000 and up that can log in) and the accounts in group www-data, which is where
/// the emulator's account lives. Other service accounts (daemon, mysql, build accounts without a login ...) are left out.
pub async fn list(server: &dyn Server) -> Result<Vec<ServerUser>> {
    let result = server.run_as_root(LIST).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("Could not read the users of the server: {}", result.error.trim())).into());
    }
    Ok(parse_list(&result.output))
}

/// The accounts in the output of the list script, root first, then the others by name.
pub fn parse_list(output: &str) -> Vec<ServerUser> {
    let lines: Vec<&str> = output.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let www: HashSet<&str> = lines
        .iter()
        .find_map(|l| l.strip_prefix("www="))
        .unwrap_or("")
        .split(',')
        .filter(|m| !m.is_empty())
        .collect();
    let mut users = Vec::new();
    for line in lines.iter().filter_map(|l| l.strip_prefix("user=")) {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() < 4 {
            continue;
        }
        let Ok(uid) = parts[1].trim().parse::<i32>() else { continue };
        let user = ServerUser {
            name: parts[0].to_string(),
            uid,
            home: parts[2].to_string(),
            shell: parts[3].to_string(),
            in_www_data: www.contains(parts[0]),
        };
        if user.uid == 0 || ((1000..65534).contains(&user.uid) && user.can_log_in()) || (user.in_www_data && user.name != "www-data") {
            users.push(user);
        }
    }
    // root first, then the others by name (a stable sort, like LINQ's OrderBy).
    users.sort_by(|a, b| (a.uid != 0).cmp(&(b.uid != 0)).then_with(|| a.name.cmp(&b.name)));
    users
}

const CREATE: &str = r#"set -e
if ! id {{name}} >/dev/null 2>&1; then
  useradd --system --user-group --home-dir /var/lib/{{name}} --create-home --shell /usr/sbin/nologin {{name}}
fi
getent group www-data >/dev/null || groupadd --system www-data
usermod -aG www-data {{name}}
id {{name}}"#;

/// Creates a service account like the Emulator step does: no login shell, home in /var/lib/<name> (for the build's
/// ~/.m2) and member of www-data. An account that already exists is only added to www-data.
pub async fn create(server: &dyn Server, name: &str) -> Result<()> {
    if !ServerLayout::is_valid_account(name) || name == ROOT_ACCOUNT {
        return Err(ServerError(format!("\"{name}\" is not a usable account name.")).into());
    }
    let result = server.run_as_root(&fill(CREATE, &[("name", name)])).await?;
    if !result.succeeded() {
        let error = result.error.trim();
        let reason = if error.is_empty() { result.output.trim() } else { error };
        return Err(ServerError(format!("Creating the account {name} failed: {reason}")).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters_accounts() {
        let output = "www=emulator,simone\n\
            user=root:0:/root:/bin/bash\n\
            user=daemon:1:/usr/sbin:/usr/sbin/nologin\n\
            user=www-data:33:/var/www:/usr/sbin/nologin\n\
            user=mysql:104:/nonexistent:/bin/false\n\
            user=emulator:998:/var/lib/emulator:/usr/sbin/nologin\n\
            user=zoe:1001:/home/zoe:/bin/bash\n\
            user=simone:1000:/home/simone:/bin/bash\n\
            user=locked:1002:/home/locked:/usr/sbin/nologin\n\
            user=nobody:65534:/nonexistent:/usr/sbin/nologin\n\
            user=broken:x:/x:/bin/sh\n\
            user=short:5\n";
        let users = parse_list(output);
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["root", "emulator", "simone", "zoe"]);
        let emulator = &users[1];
        assert!(emulator.is_system() && emulator.in_www_data && !emulator.can_log_in());
        assert_eq!(emulator.home, "/var/lib/emulator");
        assert!(!users[0].is_system() && users[0].can_log_in() && !users[0].in_www_data);
        assert!(users[2].in_www_data && !users[3].in_www_data);
    }

    #[test]
    fn empty_group() {
        let users = parse_list("www=\nuser=root:0:/root:/bin/bash\n");
        assert_eq!(users.len(), 1);
    }
}
