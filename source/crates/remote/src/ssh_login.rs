//! Turns SSH password login off or back on with a drop-in file that sshd reads before the rest.
//! (SshLoginInstaller.cs)

use std::collections::HashSet;

use aio_core::keys::AuthorizedKey;

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server};

/// sshd's effective login settings (sshd -T) and whether a saved key is in root's authorized_keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshLoginState {
    pub password_login: bool,
    pub keyboard_interactive: bool,
    pub permit_root_login: Option<String>,
    pub our_file: bool,
    pub root_has_saved_key: bool,
}

impl SshLoginState {
    pub fn keys_only(&self) -> bool {
        !self.password_login && !self.keyboard_interactive
    }
}

/// sshd keeps the first value it reads and reads sshd_config.d in name order, so 00- wins.
pub const DROP_IN: &str = "/etc/ssh/sshd_config.d/00-all-in-1-keys-only.conf";

// A reload is enough for new logins. The restart fallback also keeps open sessions, this one included.
const RELOAD: &str = r#"reload_sshd() {
  if [ -d /run/systemd/system ]; then
    systemctl reload ssh 2>/dev/null || systemctl reload sshd 2>/dev/null || systemctl restart ssh 2>/dev/null || systemctl restart sshd
  elif [ -f /run/sshd.pid ]; then
    kill -HUP "$(cat /run/sshd.pid)"
  else
    service ssh reload 2>/dev/null || service sshd reload
  fi
  sleep 1
}"#;

const INSPECT: &str = r#"sshd -T 2>/dev/null | awk '$1 == "passwordauthentication" || $1 == "kbdinteractiveauthentication" || $1 == "permitrootlogin" { print $1 "=" $2 }'
echo "ours=$([ -f {{DropIn}} ] && echo yes)"
echo "keys=$(awk '{ for (i = 1; i < NF; i++) if ($i ~ /^(ssh-|ecdsa-|sk-)/) { print $(i + 1); break } }' /root/.ssh/authorized_keys 2>/dev/null | paste -sd' ' -)""#;

pub async fn inspect(server: &dyn Server, saved_keys: &[AuthorizedKey]) -> Result<SshLoginState> {
    let script = fill(INSPECT, &[("DropIn", DROP_IN)]);
    let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
    Ok(state_from(&values, saved_keys))
}

fn state_from(values: &ScriptValues, saved_keys: &[AuthorizedKey]) -> SshLoginState {
    let root_keys: HashSet<&str> = values.or_empty("keys").split(' ').filter(|k| !k.is_empty()).collect();
    SshLoginState {
        password_login: values.get("passwordauthentication") != Some("no"),
        keyboard_interactive: values.get("kbdinteractiveauthentication") != Some("no"),
        permit_root_login: values.string("permitrootlogin"),
        our_file: values.yes("ours"),
        root_has_saved_key: saved_keys.iter().any(|k| root_keys.contains(k.base64.as_str())),
    }
}

const DISABLE_PASSWORD_LOGIN: &str = r#"{{Reload}}
c=/etc/ssh/sshd_config
[ -f "$c" ] || fail "$c is missing: is OpenSSH server installed?"
backup="$c.bak-$(date +%Y%m%d-%H%M%S)"
cp "$c" "$backup"
undo() {
  rm -f {{DropIn}}
  cp "$backup" "$c"
  sshd -t 2>/dev/null && reload_sshd
  echo "  Undone: password login works as before."
}
install -d -m 755 /etc/ssh/sshd_config.d
if ! grep -Eq '^[[:space:]]*Include[[:space:]]+/etc/ssh/sshd_config.d/\*\.conf' "$c"; then
  echo "> $c: adding Include /etc/ssh/sshd_config.d/*.conf at the top"
  sed -i '1i Include /etc/ssh/sshd_config.d/*.conf' "$c"
fi
echo "> {{DropIn}}"
cat > {{DropIn}} <<'CONF'
# Written by All-in-1 Installer (SSH login step): only SSH keys can log in.
# Lost your key? Log in on the console of your hosting provider (KVM / VNC / rescue) and delete this file,
# then: systemctl reload ssh
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin prohibit-password
PubkeyAuthentication yes
CONF
chmod 644 {{DropIn}}
if ! out=$(sshd -t 2>&1); then
  echo "$out" | sed 's/^/  /'
  undo
  fail "sshd -t refuses the new settings (see above); nothing was changed."
fi
echo "> Reloading sshd (open sessions stay connected)"
reload_sshd
effective=$(sshd -T 2>/dev/null | awk '$1 == "passwordauthentication" || $1 == "kbdinteractiveauthentication" || $1 == "permitrootlogin" { print "  " $1 " " $2 }')
echo "$effective"
if echo "$effective" | grep -Eq '(passwordauthentication|kbdinteractiveauthentication) yes'; then
  undo
  fail "sshd still allows passwords: another setting comes first. Nothing was changed."
fi"#;

/// Writes the keys-only drop-in, validates and reloads sshd, and puts everything back when a check fails.
pub async fn disable_password_login(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(DISABLE_PASSWORD_LOGIN, &[("Reload", RELOAD), ("DropIn", DROP_IN)]);
    script::run(server, &script::with_prelude(&body), log, "Turning password login off failed", None).await
}

const ALLOW_PASSWORD_LOGIN: &str = r#"{{Reload}}
if [ -f {{DropIn}} ]; then
  echo "> Removing {{DropIn}}"
  rm -f {{DropIn}}
  sshd -t 2>&1 | sed 's/^/  /'
  reload_sshd
else
  echo "  {{DropIn}} is not there: nothing to undo."
fi
sshd -T 2>/dev/null | awk '$1 == "passwordauthentication" || $1 == "kbdinteractiveauthentication" || $1 == "permitrootlogin" { print "  " $1 " " $2 }'"#;

/// Removes the drop-in so password login follows the rest of the sshd configuration again.
pub async fn allow_password_login(server: &dyn Server, log: Log<'_>) -> Result<()> {
    let body = fill(ALLOW_PASSWORD_LOGIN, &[("Reload", RELOAD), ("DropIn", DROP_IN)]);
    script::run(server, &script::with_prelude(&body), log, "Allowing password login failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_state() {
        let saved = [AuthorizedKey::new("ssh-ed25519", "AAAAB", "me"), AuthorizedKey::new("ssh-rsa", "AAAAC", "")];
        let state = state_from(
            &ScriptValues::parse("passwordauthentication=no\nkbdinteractiveauthentication=no\npermitrootlogin=prohibit-password\nours=yes\nkeys=AAAAX AAAAC\n"),
            &saved,
        );
        assert_eq!(
            state,
            SshLoginState {
                password_login: false,
                keyboard_interactive: false,
                permit_root_login: Some("prohibit-password".into()),
                our_file: true,
                root_has_saved_key: true
            }
        );
        assert!(state.keys_only());

        // Unknown values count as allowed (sshd -T failed).
        let unknown = state_from(&ScriptValues::parse("ours=\nkeys=AAAAX"), &saved);
        assert!(unknown.password_login && unknown.keyboard_interactive && !unknown.keys_only());
        assert_eq!(unknown.permit_root_login, None);
        assert!(!unknown.our_file && !unknown.root_has_saved_key);
        let keyboard = state_from(&ScriptValues::parse("passwordauthentication=no\nkbdinteractiveauthentication=yes"), &[]);
        assert!(!keyboard.keys_only());
    }
}
