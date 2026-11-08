# All-in-1 Manager

A desktop installer, written in Rust, that sets up a **full hotel server** on an **Ubuntu Server or Debian**
machine over SSH:

| Component | Repository |
|-----------|------------|
| Polaris Emulator (Java 25 game server + MariaDB) | <https://github.com/duckietm/Polaris-Emulator> |
| Octane (browser client) | <https://github.com/duckietm/Octane> |
| Octane Renderer | <https://github.com/duckietm/Octane-Renderer> |
| AtomCMS (addon, optional) | <https://github.com/ObjectRetros/atomcms> |

The GUI looks like a Habbo hotel: hotel-blue sidebar, the yellow of the hotel logo, green "go" buttons, panels with a
dark edge and a hard shadow like the hotel's windows, a hotel view on the dashboard (blue sky with outlined clouds by day, stars at night, and a hotel
scene), Ubuntu for text (as habbo.com and the client use) and Pixelify Sans for titles in the spirit of
the hotel's pixel font. Both fonts are free (Ubuntu Font Licence, SIL Open Font License) and ship in
`source/crates/app/assets/fonts` with their licences. The whole hotel view is drawn by the installer itself (sky, clouds,
stars and an isometric hotel), so no picture of the game is shipped. The layout (sidebar, cards, status pill, console
log) comes from the [All-in-1 Converter](https://github.com/duckietm/all-in-1-converter).

The window speaks **English, Italiano, Français, Deutsch, Nederlands, Español, Português, Türkçe and Suomi**
(the **flag at the top right**, next to the day / night switch, or **Settings → Language**; the first start follows
the system language). A choice applies at once and is remembered. The logs of the steps stay in English: they
are what the server prints, and what you paste into a question.

> This is the Rust version of the installer (2.x). It does the same as the C# installer 1.0.x, command for command
> (see [Parity with the C# installer](#parity-with-the-c-installer)), as one program without .NET: one `.exe` for
> Windows and one file for Linux. `configuration/` (settings, SSH keys, database passwords) and the encrypted settings
> backups are the same files, so an existing configuration keeps working.

**Settings → Look** switches between this **Hotel** look and the **Professional** look of the All-in-1 Converter
(indigo, soft cards). It is stored in `configuration/settings.json` and applies at the next start; **Restart
now** starts the installer again right away (it waits while a step runs). Both looks come in day and night: the
switch at the top right (a sun with a cloud, or a moon with stars) changes it at once, and **Settings → Day or night**
also has **Follow the system**, the default.

![Dashboard](public/docs/screenshots/dashboard.png)

| Professional look | Dark |
|---|---|
| ![Professional look](public/docs/screenshots/professional.png) | ![Dark](public/docs/screenshots/dashboard-dark.png) |

Once you are logged in, the dashboard's **Full server installation** card shows what is on the server: per component
the build version (the emulator's JAR, Octane's and the renderer's `package.json`), the branch, commit and date of
its clone, and whether GitHub has something new. That comes from the same `git fetch` as **Tools → Check for
updates** (right after logging in, and again after every update; **Check again** fetches once more), counted
against the branches chosen in **Settings → Repository branches**. A component that is behind gets
**N NEW COMMITS** and an **Update** button, which opens Tools and runs that update (Update emulator, Update Octane,
or Update AtomCMS) with its question and progress there. Octane and Octane-Renderer are updated and built together,
so both rows run Update Octane. AtomCMS gets a row of its own once it is installed.

## At a glance

- **One program, nothing to install**: one `.exe` for Windows and one file for Linux, with fonts, translations and
  the client images inside. It updates itself when a new release is out.
- **The whole server in one wizard**: SSH key, NGINX, PHP 8.5, MariaDB 12.3, Node.js, the Polaris / Octane clones,
  the emulator as a service, Traefik with Let's Encrypt (and Cloudflare), the Octane client, hardening and SSH keys
  only. Every step says what it will do first, and can be run again.
- **Addons**: AtomCMS, EpicNext or your own CMS, Gitea, Fail2Ban, CrowdSec, the default catalogue and a SQL Editor over the SSH
  tunnel.
- **Assets** (its own section in the sidebar): Habbo assets (Habbo's own furniture, clothes, effects, badges and
  game data as .hab and JSON), Nitro retro (a retro's custom furniture and clothes), Asset tools (decompile and
  compile .hab, SWF / NITRO to HAB), Deploy Assets and an Asset Explorer.
- **Running the hotel**: restarts, updates of the emulator, Octane, AtomCMS and the containers, database backup and
  import (also PlusEMU), live server status, the emulator's wired CPU, an SSH terminal in the window, a **Health
  check** with a fix for each problem, and a **History** of every run with its log.
- **Safe to try**: `--dry-run` shows every command a step would send, without a server. The commands are the C#
  installer's own, checked byte for byte.

## Quick start

1. Download `all-in-1-manager-windows-x64.exe` or `all-in-1-manager-linux-x64` from the
   [releases](https://github.com/duckietm/All-in-1-Manager/releases) and start it (on Linux: `chmod +x` first).
2. Log in to your server: its address, `root` (or a user with sudo) and the password or an SSH key.
3. **Install New Server** and follow the steps. Each one explains what it does; **Skip** leaves it out.
4. Optional: a CMS, Gitea, Fail2Ban, CrowdSec or the default catalogue under **Addons**, then **Habbo assets** and
   **Deploy Assets** (sidebar → ASSETS) for the game's files.
5. Afterwards **Tools** runs the hotel: **Health check** first when something seems wrong.

To look around first: `all-in-1-manager --dry-run` logs in to nothing and only shows the commands.

**Does not open (a Windows VM)?** On Windows the window is drawn with DirectX 12 (wgpu), which works without a GPU
too (VMware SVGA 3D, Hyper-V, Microsoft's software renderer). If that fails, the installer starts itself again with
OpenGL; if neither works it says so in a message box, and the reason is in `configuration/start-error.log` next to the
program. Choose one by hand with `--renderer=wgpu` or `--renderer=glow`.

## Contents

- [Requirements](#requirements)
- [Install New Server, step by step](#install-new-server-step-by-step)
- [Addons](#addons): [CMS](#cms), [Gitea](#gitea), [Fail2Ban](#fail2ban), [CrowdSec](#crowdsec),
  [Habbo assets](#habbo-assets), [Nitro retro](#nitro-retro-custom-assets), [Asset tools](#asset-tools), [Deploy Assets](#deploy-assets), [Asset Explorer](#asset-explorer), [SQL Editor](#sql-editor)
- [Settings](#settings) · [Debug](#debug) · [Tools](#tools) · [Health check and History](#health-check-and-history)
- [Updating the installer itself](#updating-the-installer-itself) · [Building](#building) ·
  [Project layout](#project-layout) · [Credits](#credits)

## Requirements

**On your computer** (Windows 10/11 x64 or Linux x64): nothing. The installer is one file, with everything it
needs inside (fonts, the client images for Deploy Assets). Download `all-in-1-manager-windows-x64.exe` or
`all-in-1-manager-linux-x64` from the [releases](https://github.com/duckietm/All-in-1-Manager/releases), or
[build it](#building). On Linux it needs a desktop (X11 or Wayland) and OpenGL.

**The server**: a fresh Ubuntu Server (22.04 or newer) or Debian (12 or newer) machine you can reach over SSH as
root or as a user with sudo rights. Everything else (NGINX, PHP, MariaDB, Java, Node.js, Docker ...) is installed
by the wizard.

## Install New Server, step by step

### Logging in (before the steps)

When the program starts, it asks for the server first. **One login is used for everything**: Install New Server,
the addons and the tools. It stays open until you close the program or the server drops the connection.

1. **Server IP / hostname**: e.g. `192.168.1.10` or `hotel.example.com`. Add `:2222` for a different SSH port
   (`[::1]:2222` for IPv6).
2. **User**: `root`, or a user with sudo rights.
3. **Log in with**:
   - **Password**: also used for sudo when the account is not root, or
   - **SSH key**: a private key from `configuration/keys` (the default key from Settings, or else one in a folder
     named after the server, is selected), **Browse** for another file, or **Import key ...**: copies your private
     key (OpenSSH, PEM or PuTTY `.ppk`) to `configuration/keys/<server>/` with its public key next to it. Without any
     key in the configuration, the login says so and Import key is the highlighted button. The key and its password
     are checked on this computer before connecting; the key never leaves it. The user of a cloud image (`ubuntu`,
     `debian` ...) needs sudo without a password.

With your servers and a default key in **Settings**, the default server is filled in and the others are in the
server list to choose from (or type another one). **Not now** opens the program without a server:
the pages that need one show a **Log in** button, and so does the status at the top right (**Not connected · Log in**).
**Disconnect** (next to the status at the top right) closes the login, for example to log in to another server; it
waits while a step or tool is running.

![Login](public/docs/screenshots/login.png)

Logging in:

- logs in over SSH (password, keyboard-interactive / PAM, or the SSH key),
- reads `/etc/os-release` and refuses anything that is not Ubuntu or Debian,
- switches to root when the user is not root:
  - passwordless sudo (`NOPASSWD`) is detected and used,
  - otherwise the password is sent to `sudo -S` over **stdin**, so it never shows up in `ps`,
  - a user that is not in the `sudo` group gets a clear error with the fix
    (`usermod -aG sudo <user>`),
- confirms root access by running `id -un` as root.

A failed login shows the reason in the login window, which stays open to try again. When the server drops the
connection (a reboot, a network problem), the installer notices within a few seconds and opens the login again,
filled in with the same server, user and key (never the passwords), as soon as no step is running.

Pages leave with **Back**, which returns to the dashboard and keeps the login open; a wizard continues where it was
the next time you open it. Logging in again (after a lost connection) starts the wizards over.

Every step uses `Server::run_as_root` (`source/crates/remote/src/server.rs`), which always runs as root: directly when logged in as root,
or through sudo. Files copied over SFTP into folders only root can open (the database backups) go through a private
staging folder in `/tmp` when the login is not root.

### Step 1 – SSH key for root

**Install New Server** starts with the SSH key step, which puts a key in `/root/.ssh/authorized_keys`, so the next
login can be root with that key. Choose one:

- **Generate a new key pair**: Ed25519 (recommended) or RSA 4096, in the standard OpenSSH format
  (works with `ssh`, the installer itself, and PuTTYgen via *Conversions → Import key*). You must set a
  **key password** (at least 8 characters, typed twice). The private key is encrypted with it exactly
  like `ssh-keygen -N` does (aes256-ctr, bcrypt KDF with 16 rounds). The password is never stored and
  is cleared from the form after each run. The keys are saved in the **configuration directory** on
  this computer *before* anything changes on the server: `id_ed25519` (OpenSSH), `id_ed25519.pub`, and
  `id_ed25519.ppk`, the same key for PuTTY tools (plink in HeidiSQL, Pageant, WinSCP), encrypted with the same
  password. It is PPK version 2 (aes256-cbc, HMAC-SHA-1), which every PuTTY since 0.52 loads; version 3 would need
  Argon2. A key made before the `.ppk` existed can be converted with PuTTYgen (*Conversions → Import key*, then
  *Save private key*).
- **Use my own public key**: paste it or load a `.pub` file. The OpenSSH one-line format and the
  PuTTYgen `---- BEGIN SSH2 PUBLIC KEY ----` format both work. A pasted private key is refused.
  A copy of the public key is kept in the configuration directory.

![SSH key options](public/docs/screenshots/ssh-key-options.png)

If the server already has keys, the installer lists them (comment, type, fingerprint, and whether your
key is already one of them) and asks:

- **Clean all keys and place this one**: only the new key remains.
- **Add my key to it**: keeps the existing keys, so the server has several (no duplicates).
- **Abort installation**: changes nothing on the server. A key pair generated for this run is deleted again.

After an abort you choose **Abort** (leave the installation and go back to the dashboard; it starts over the next
time) or **Start over** (the SSH key step again). The login stays open either way.

Before every change, the old file is backed up on the server as
`/root/.ssh/authorized_keys.bak-<date>-<random>`. The installer writes `/root/.ssh` with mode 700
and `authorized_keys` with mode 600, both owned by root, and checks that the key ended up in the file.
For a generated key it then opens a second SSH session **as root with the new key** to prove it works.
If that fails, it reports why (for example `PermitRootLogin no` in sshd). The second session only
trusts the host key seen during the first login.

#### The configuration directory

`configuration/` is created next to the installer (falling back to
`%LOCALAPPDATA%\All-in-1 Manager\configuration` or `~/.local/share/All-in-1 Manager/configuration`
when that folder is read-only). Keys are stored per server:

```
configuration/
  keys/
    <server>/
      id_ed25519              private key, encrypted with your key password (owner-only on Linux/macOS)
      id_ed25519.pub          public key
      provided_ed25519.pub    copy of a public key you provided
  database/
    <server>/
      mariadb.txt             MariaDB root, cms and emulator passwords (owner-only)
```

Existing files are never overwritten: a second key gets a timestamp, e.g. `id_ed25519_20261024-101500`.
**Keep this folder safe: it holds private keys and database passwords.** It is in `.gitignore`.

Log in later with the generated key (ssh asks for the key password):

```bash
ssh -i "configuration/keys/<server>/id_ed25519" root@<server>
```

### Step 2 – NGINX mainline

After the SSH key, **Next: NGINX** installs the web server:

1. **Check the server**: distribution, any installed NGINX, and what listens on port 80. If another
   program (Apache, ...) holds port 80, it stops here and nothing is changed.
2. **Add the nginx.org mainline repository**: `https://nginx.org/packages/mainline/<ubuntu|debian>`.
   The downloaded signing key must contain nginx.org's published key `573BFD6B3D8FBC641079A6ABABF5BD827BD9BF62`,
   and any key outside nginx.org's known set is refused. The repository is pinned (priority 900), so
   apt always takes NGINX from nginx.org.
3. **Install NGINX mainline** and check that the installed version really comes from nginx.org.
4. **Debian layout**: `nginx-common` itself can't be installed, because nginx.org's package declares
   `Conflicts/Replaces: nginx-common`, and the old repository that combined both (Ondřej Surý's) no
   longer exists. So the installer creates the same layout: `/etc/nginx/sites-available`,
   `sites-enabled` (included from `nginx.conf`), `snippets` and `proxy_params`. The nginx.org default site
   moves to `sites-available/default` and is enabled. On a server without IPv6 the `[::]:80` listen is
   disabled, because otherwise NGINX would not start.
5. **Start and test**: `nginx -t`, enable and start at boot, open 80/443 when `ufw` is active, and require
   HTTP 200 from `http://127.0.0.1/`.

![NGINX](public/docs/screenshots/nginx-intro.png)

If Ubuntu/Debian's own NGINX is installed, you choose **Replace it with NGINX mainline** (`/etc/nginx` is
saved to `/root/nginx-backup-<date>.tar.gz`, the old packages are purged, and the configuration starts fresh)
or **Abort installation**. If nginx.org's NGINX is already there, the step upgrades it and keeps your
`nginx.conf` changes. Running the step again is safe.

Long commands stream their output live into the log (`Server::run_as_root_streaming`).
**Copy log** in the top right corner of the log puts it on the clipboard, so a failure can be pasted into a
message instead of screenshotted. Select lines first to copy only those; Ctrl+C does the same. The button is
on every log in the program: the install steps, the connect page and the Tools.

Before every `apt-get update` the installer checks the **server clock** against the `Date` header of
`deb.debian.org`. A clock that is more than 2 minutes off is corrected and automatic time sync (NTP) is switched
on, because apt refuses repository files that seem to come from the future
(`Release file ... is not valid yet`).

Every `apt-get` (and `dpkg -i`) also **waits while another apt runs**, instead of stopping with
`Could not get lock /var/lib/dpkg/lock-frontend`: on a fresh Ubuntu, `unattended-upgrades` installs updates for a
while after the first boot. The log says it is waiting (a line every minute); after 30 minutes the step stops and
asks to wait or reboot. The locks are read from `/proc/locks`, so a quiet server never waits.

### Step 3 – PHP 8.5 with PHP-FPM

After NGINX, **Next: PHP** installs:

```
php8.5-fpm php8.5 php8.5-common php8.5-mysql php8.5-xml php8.5-xmlrpc php8.5-curl php8.5-gd
php8.5-imagick php8.5-cli php8.5-mbstring php8.5-soap php8.5-zip php8.5-intl php8.5-bcmath unzip
```

PHP **8.5.12 or newer** is required because older releases have security problems. The installer refuses
to finish with an older version.

1. **Check the server**: installed PHP version, missing packages, other PHP versions (those are left alone).
2. **Add the repository** as in `packages.sury.org/php/README.txt`, which works for Debian (bookworm, trixie)
   and Ubuntu (jammy, noble, resolute). Before `dpkg -i`, the downloaded `debsuryorg-archive-keyring.deb`
   is unpacked and must hold Sury's key `15058500A0235D97F5D10063B188E2B695BD4743`. `php8.5*` is pinned to
   packages.sury.org, so an older distro 8.5 can never win.
3. **Install** the packages and require `PHP_VERSION` ≥ 8.5.12; `php` on the command line points to 8.5.
4. **Check** that all extensions load (`php8.5 -m`) and that `php-fpm8.5 -t` passes.
5. **Start PHP-FPM** at boot. When NGINX is installed: user `nginx` joins group `www-data` (owner of
   `/run/php/php8.5-fpm.sock`), `snippets/fastcgi-php.conf` is created (as in Debian's nginx-common), and a
   temporary site on `127.0.0.1:8085` must serve a PHP page through NGINX. The test site is removed afterwards.

![PHP](public/docs/screenshots/php-intro.png)

In an NGINX site:

```nginx
location ~ \.php$ {
    include snippets/fastcgi-php.conf;
    fastcgi_pass unix:/run/php/php8.5-fpm.sock;
}
```

### Step 4 – MariaDB 12.3

After PHP, **Next: MariaDB** installs the database:

1. **Check the server**: an installed MariaDB (Ubuntu/Debian's or another mariadb.org version) is upgraded
   after you agree, and **all databases are first saved to `/root/mariadb-backup-<date>.sql.gz`**. If MySQL
   is installed, the step stops: MariaDB can't be installed next to it.
2. **Add the repository** `https://dlm.mariadb.com/repo/mariadb-server/12.3/repo/<ubuntu|debian>` (Ubuntu
   22.04/24.04/26.04, Debian 11/12/13). The signing key from `mariadb.org/mariadb_release_signing_key.pgp`
   must be `177F4010FE56CA3336300305F1656F24C74CD1D8`; the repository is pinned.
3. **Install MariaDB 12.3** and check that the *running* server is 12.3. A server still running the replaced
   old program (when package scripts may not restart services) is shut down cleanly first. Then
   `mariadb-upgrade` runs.
4. **Secure it**, with the same answers as your `mariadb-secure-installation`: unix_socket switch **n**, change
   root password **n**, remove anonymous users **Y**, disallow remote root **n**, remove test database **Y**,
   reload privileges **Y**. Then `bind-address = 0.0.0.0` in `/etc/mysql/mariadb.conf.d/50-server.cnf` (a backup
   of the file is kept). Port 3306 is **never** opened in the firewall: connect through an SSH tunnel (see Harden).
   `root@localhost` keeps its socket login and also accepts the root password, because a tunnel arrives from
   127.0.0.1, which MariaDB counts as `localhost`.
5. **Create the users and database** with **generated passwords** (32 letters and digits):
   ```sql
   root@'%'  -- ALL PRIVILEGES ON *.* WITH GRANT OPTION
   cms@'%'   -- EXECUTE, SELECT, SHOW VIEW, ALTER, ALTER ROUTINE, CREATE, CREATE ROUTINE, CREATE TEMPORARY TABLES,
             -- CREATE VIEW, DELETE, DROP, EVENT, INDEX, INSERT, REFERENCES, TRIGGER, UPDATE, LOCK TABLES ON habbo.*
   CREATE SCHEMA habbo;
   ```
   `root@localhost` keeps its socket login (no password) for administering MariaDB on the server itself.
   The SQL goes to the server over stdin, so the passwords never appear in a command line (`ps`), and
   password hashes are masked in the log.
6. **Test the logins**: root with its password over the network address (that is `root@'%'`), root locally
   over the socket, and cms remotely, over 127.0.0.1 and over the socket, including creating a table in `habbo`.

![MariaDB](public/docs/screenshots/mariadb-intro.png)

The passwords are saved **before** anything changes on the server, in
`configuration/database/<server>/mariadb.txt` (owner-only):

```
host=192.168.0.155
port=3306
database=habbo
root_user=root
root_password=...
cms_user=cms
cms_password=...
```

Running the step again reuses these passwords. If the users exist on the server but there is no saved file
(e.g. from another PC), new passwords are set and saved. The step says so before you install.

### Step 5 – Node.js 26, Composer and Yarn

After MariaDB, **Next: Node.js** installs the build tools, following your commands:

1. **Check the server**: Composer needs PHP, so without PHP the step stops before changing anything.
2. **Add NodeSource** `https://deb.nodesource.com/node_26.x nodistro main` with the key in
   `/etc/apt/keyrings/nodesource.gpg`. The key must be `6F71F525282841EEDAF851B42F59B5F99B1BE0B4` (NSolid), and
   `nodejs` is pinned to NodeSource.
3. **Install Node.js 26** (`apt-get install nodejs`, which includes npm) and check the major version.
4. **Install Composer**: the latest stable `composer.phar` from `getcomposer.org/download`, checked against its
   published SHA-256, to `/usr/bin/composer`. Not `composer-setup.php`: it downloads the phar itself without a
   timeout and can hang forever on a broken IPv6 route. All installer downloads use curl with timeouts and retry
   over IPv4.
5. **Install Yarn**: `npm install -g corepack`, `corepack enable`, and Yarn stable as the default outside
   projects. Each project uses the Yarn in its `package.json` (Octane: `yarn@4.18.0`).
6. **Check the versions**: `node`, `npm`, `yarn` and `composer --version`.

![Node.js](public/docs/screenshots/node-intro.png)

The step uses and tests the tools it installs in `/usr/bin`, also when an older `node`, `npm` or `composer`
(e.g. a hand-installed one in `/usr/local/bin`) comes first in root's `PATH`. Those older copies are reported
as a warning with their paths, so typing `node` really runs Node.js 26 after you remove them; the installer
does not delete them itself.

### Steps 6–8 – Clone Polaris, Octane and Octane-Renderer

| Step | Repository | Folder on the server |
|------|------------|----------------------|
| Polaris | https://github.com/duckietm/Polaris-Emulator | `/var/www/emulator` |
| Octane | https://github.com/duckietm/Octane | `/var/www/Octane` |
| Renderer | https://github.com/duckietm/Octane-Renderer | `/var/www/Octane-Renderer` |

Each step clones the full history (so `git pull` can update it later) and installs git first when needed. The
log keeps only the readable progress lines.

Every step has a **Branch** choice: **main** (stable, the version the hotel runs on) or **dev** (the newest
changes, not always stable). The choice is remembered per repository in `configuration/settings.json`, so the
next installation starts with it, and **Settings → Repository branches** sets the same thing for all three
without logging in to a server. The name is matched without case against the branches the repository really
has, because Polaris writes it as `dev` and Octane and the Renderer as `Dev`.

- **Folder missing or empty**: `git clone --branch <the branch you picked>`.
- **Folder already holds that repository** (`origin` matches, also as `git@github.com:...`): the step shows the
  branch, commit, locally changed files and new commits, and offers **Skip** or **Update (git pull)**. The update
  is `git pull --ff-only`, and it is refused when files tracked by git were changed, so local work is never lost.
  Is the clone on another branch than the one you picked? Then the step says so and switches to it
  (`git fetch` + `git checkout`) before pulling.
- **Folder holds something else** (other files, or a clone of another repository): **Move it aside and clone**
  renames it to `<folder>.bak-<date>` (nothing is deleted), or **Abort installation**.

![Clone](public/docs/screenshots/clone-intro.png)

### Step 9 – Build and run the Polaris emulator

After the clones, **Next: Emulator** builds `/var/www/emulator/Emulator` and runs it as a service. On the intro
page you choose:

- **Timezone** for `config.ini` (`serverTimezone`): a dropdown with every IANA timezone the server knows
  (`timedatectl list-timezones`; this computer's list when the server has none), each with its current UTC
  offset. Type the first letters to jump. Defaults to **Europe/Amsterdam**, or the timezone already in `config.ini`.
- **Memory** (`-Xmx`): 1–16 GB, defaulting to 4 GB (less on small servers; the server's RAM is shown).
- **Websocket whitelist** (`ws.whitelist`): hosts the client may connect from; add your hotel's domain
  (`hotel.com,*.hotel.com`). Polaris's default is `*` (any site); the installer replaces it.

What it does:

1. **JDK 25**: `openjdk-25-jdk-headless` (Ubuntu 22.04/24.04/26.04, Debian 13), or **Eclipse Temurin 25** from
   Adoptium (key `3B04D753C9050D9A5D343F39843C48A565F8F04B` checked) where the distribution has no OpenJDK 25
   (Debian 11/12). `pom.xml` requires JDK 25–26, so the scripts name that JDK explicitly; the server's default
   `java` is not changed.
2. **Account**: system user `emulator` (no login shell, home `/var/lib/emulator`), member of `www-data`. It owns
   `/var/www/emulator` and the log folder `/var/log/emu` (`2750 emulator:www-data`, so logs are group-readable).
3. **Build** with `/var/www/emulator/Emulator/build.sh`, run as `emulator`:
   ```sh
   ./build.sh               # mvn -B -ntp clean package -DskipTests -f pom.xml   (about 1 minute)
   ./build.sh --with-tests  # also the 4000+ unit tests                          (about 4 minutes)
   ```
   `pom.xml` requires Maven 3.9.11+, which is newer than apt's `maven` (3.8.7 on Ubuntu 24.04). The installer
   downloads the version the Maven Wrapper asks for (`.mvn/wrapper/maven-wrapper.properties`, now 3.10.0) to
   `/opt/apache-maven-<version>` itself: from Maven Central, else `dlcdn.apache.org` / `archive.apache.org`, with
   timeouts and an IPv4 retry, and checks the sha256 from that file. (`mvnw` downloads silently and without a
   timeout, which could hang the step forever.) `build.sh` falls back to `./mvnw` when that folder is missing.
   While Maven runs, the log shows a progress line every 30 seconds; the build is stopped after 30 minutes.
   The first build downloads about 150 MB of libraries to `/var/lib/emulator/.m2`. The JAR is now called
   `Polaris-<version>-jar-with-dependencies.jar`.
4. **Database account, config.ini, service**:
   - MariaDB account `emulator@localhost` with `ALL PRIVILEGES ON habbo.*` and a generated password, saved as
     `emulator_user` / `emulator_password` in `configuration/database/<server>/mariadb.txt` (sent over stdin).
   - `Emulator/config.ini` (`640 emulator:www-data`) from the repository's `config.ini.example`: `db.*` for the
     emulator account, `db.params=?characterEncoding=utf8&useSSL=false&serverTimezone=<your timezone>`, and
     generated secrets for `nitro.secure.master_key` and `login.remember.jwt.secret`. On a re-run only the
     database settings change; the rest of your `config.ini` is kept (with a `.bak-<date>` copy).
   - Start script `Emulator/emulator`: runs the newest JAR in `target/` with JDK 25 and your memory, logs to
     `/var/log/emu/emulator.log`, keeps the previous log as `emulator.log.<time>` (30 days).
   - `/etc/systemd/system/emulator.service`: `User=emulator`, `SupplementaryGroups=www-data`, after and requiring
     `mariadb.service`, `Restart=on-failure`, enabled at boot. `WorkingDirectory` is `Emulator/` (not `target/`),
     so `config.ini` survives `mvn clean`.
5. **Start**: the first start creates all database tables (Polaris migrations); then `console.mode = 0` and
   `ws.whitelist` (and the old `websockets.whitelist`) are set in `emulator_settings`, and it restarts when they
   changed. It waits for `Polaris has successfully loaded.` and checks port 3000 (opened in ufw when active).

![Emulator](public/docs/screenshots/emulator-intro.png)

```sh
systemctl status emulator          # running?
systemctl restart emulator         # after changing config.ini
tail -f /var/log/emu/emulator.log  # follow the log
cd /var/www/emulator/Emulator && sudo -u emulator ./build.sh && systemctl restart emulator   # rebuild
```

### Step 10 – Web access: Docker and Traefik

The step asks three questions:

1. **Do you use Cloudflare?** yes / no
2. **http or https?**
3. **The hotel's address**: a domain name (`example.com`) or an IP address. Typing `https://example.com/`
   also works: the scheme is taken from it.

https and Cloudflare need a domain name (Let's Encrypt gives no certificates for IP addresses).

What it sets up, for example for `https://example.com`:

```
https://example.com/...      Traefik (80/443) -> NGINX      http://127.0.0.1:85
https://example.com/ws       Traefik          -> Polaris    http://127.0.0.1:2096  (game websocket)
https://example.com/api/...  Traefik          -> Polaris    http://127.0.0.1:2096  (login, CMS API ...)
```

1. **Docker Engine** as on [docs.docker.com/engine/install/ubuntu](https://docs.docker.com/engine/install/ubuntu/)
   (and `/debian`): conflicting packages (`docker.io`, `podman-docker`, `containerd`, `runc` ...) are removed after
   you agree, the key `9DC858229FC7DD38854AE2D88D81803C0EBFCD88` is checked, `docker.sources` is added, and
   `docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin` are installed.
2. **NGINX** gets the site `/etc/nginx/sites-available/hotel` (`source/crates/remote/src/hotel_site.rs`; the Octane step writes the same
   file with its own root), modelled on a production Octane site:
   - `listen 127.0.0.1:85` only (so it is reachable through Traefik only), `server_name` with and without `www.`,
     `real_ip` from Traefik, `server_tokens off`, `autoindex off`, security headers (`X-Content-Type-Options`,
     `Referrer-Policy`, `Permissions-Policy`, and HSTS with https), logs in `/var/log/nginx/hotel/`;
   - `/configuration/` and `/.vite/manifest.json` never cached, hashed build files in `assets/` cached for a year;
   - `/gamedata/` from `/var/www/gamedata` (camera photos in `/gamedata/camera/`) and `/custom/` from
     `/var/www/gamedata/custom`, with CORS (and `OPTIONS` preflight) only for the hotel's own origin
     (`map $http_origin`), and images / `.nitro` / `.hab` cached for 30 days. `nginx` is added to the `www-data`
     group, so it can read the photos the emulator writes (mode 640, group `www-data`).

   The default site is disabled; another site that still listens on 80/443 stops the step with its file name.
3. **Traefik v3** in Docker (`/opt/traefik`): `compose.yaml` (host network, `restart: unless-stopped`),
   `traefik.yml`, `dynamic/hotel.yml` (the routes above) and `hotel.env` (your answers). With https, http
   redirects to https and the Let's Encrypt certificate (HTTP challenge) is renewed automatically. With
   Cloudflare, `X-Forwarded-For` is only trusted from Cloudflare's published IP ranges; without it, Traefik
   writes the header itself, so it cannot be faked. Ports 80 and 443 are opened in ufw when it is active.

   **www**: a hotel on a domain of its own also answers on its second name, `example.com` on `www.example.com`
   and `www.example.com` on `example.com` (also for two-part endings such as `example.co.uk`). A subdomain
   (`test.example.com`), `localhost` and an IP address are the only name. The second name **redirects** (301) to
   the address you entered, keeping the path, so cookies, Polaris' websocket whitelist and the CMS' own URL keep
   one name; it has a router and certificate of its own in `dynamic/hotel-www.yml`, so it can never cost the
   address its certificate. That file is only written when the second name has a DNS record that points where the
   address points (any record with Cloudflare); otherwise the log says which record to add (an A record, or a
   CNAME to the address for `www`), and running the step again adds the redirect.
4. **Polaris**: the address is added to `ws.whitelist` (websocket origins and CORS for `/api`), and
   `ws.ip.header = X-Forwarded-For`, so bans and rate limits see the visitor's IP; with Cloudflare also
   `ws.ip.header.trusted` = Cloudflare's ranges (see **The players' real IP behind Cloudflare** below). The emulator
   restarts when that changed. With Cloudflare the weekly refresh of Cloudflare's ranges is set up too.
5. **Test** on the server through Traefik: the site, `/api/health` and a websocket upgrade on `/ws` (101), and
   with https it waits up to 2 minutes for the certificate. Without a certificate the step ends with a warning
   that shows Let's Encrypt's error (usually: the domain does not point to this server yet, or port 80 is closed).

![Web access](public/docs/screenshots/web-access-intro.png)

With Cloudflare: keep the DNS record **proxied** (orange cloud) and set SSL/TLS to **Full (strict)** for https.

```sh
cd /opt/traefik && docker compose logs -f traefik        # Traefik log (certificates, routing errors)
cd /opt/traefik && docker compose pull && docker compose up -d   # update Traefik
```

### Step 11 – Octane client

Uses the address from the web access step; nothing to fill in. For `https://example.com`:

1. `yarn install` in `/var/www/Octane-Renderer`, then in `/var/www/Octane` (Node.js 26 and Yarn 4 from `/usr/bin`).
2. In `/var/www/Octane/public/configuration`, `client-mode`, `hotlooks`, `infostand_backgrounds`, `news`,
   `renderer-config` and `ui-config` `.json` are created from the `.example` files (existing files are kept), and
   the URL keys are set (an existing file that changes is saved as `.bak-<time>` first).
   `renderer-config`, `ui-config` and `client-mode` **follow their `.example`** on every run (also after
   **Update Octane** pulls a newer one): a key the `.example` has and the `.json` lacks is added with the example's
   value, also inside nested settings, and the keys are put in the example's order; keys only your `.json` has stay,
   after them. A value already in the `.json` is never changed (lists neither) apart from the URL keys below. The
   data files (`news`, `hotlooks`, `infostand_backgrounds`) are left as they are.

   | File | Keys |
   |---|---|
   | `renderer-config.json` | `socket.url` = `wss://example.com/ws` (`ws://` with http), `api.url` = `https://example.com`, `asset.url` = `https://example.com/gamedata`, `image.library.url` = `…/gamedata/c_images/`, `hof.furni.url`, `furni.asset.icon.url` = `…/gamedata/icons/…`, `badge.asset.group.url` = `…/gamedata/c_images/groupbadge/badge/%badgedata%`, `badge.asset.grouparts.url` = `…/gamedata/c_images/groupbadge/badgeparts/badgepart_%part%.png`, `group.badge.url` = `%imagerdata%`, `habbicons.asset.root` = `/habbicons` (`/client/habbicons` with AtomCMS: the icons are in Octane's own build) |
   | `ui-config.json` | `camera.url` = `https://example.com/gamedata/camera`, `thumbnails.url` = `…/gamedata/camera/thumb/%thumbnail%.png`, `camera.render.mode` = `upload` |
   | `client-mode.json` | `apiBaseUrl` = `https://example.com`, `plainConfigBaseUrl` = `…/configuration/`, `plainGamedataBaseUrl` = `…/gamedata/` |

   A new file also gets `timezone.settings` from the emulator's
   timezone, and in `client-mode.json` obfuscation, secure assets and secure API off.
3. **`/var/www/gamedata`**: every folder those URLs point to is created (`config`, `furniture`, `clothes`,
   `effect`, `pets`, `room`, `icons`, `c_images/album1584`, `c_images/catalogue`, … 26 in all), owned by
   `www-data` with the group sticky (`2775`). **The assets themselves are not copied**: put them there with the
   All-in-1 download tool. The emulator writes into it as well (custom badges into `c_images/album1584`,
   `users_custom_badge_settings.badge_path`; furni editor changes into `config/*.json`), so
   `/usr/local/sbin/gamedata-permissions` gives the `www-data` group read/write access with **default ACLs**: files
   copied in later as root or another user stay writable for the emulator and readable for NGINX. Run it again if
   an upload tool reset the permissions. `news/news.json` (the login screen's news, `login.news.url`) gets a "Welcome to <address>!"
   message when it does not exist yet; your own news is never overwritten.
4. `yarn build` (and `/var/www/Octane/build.sh`, which runs both `yarn install`s and the build again: use it after
   changing the configuration or after a `git pull`).
5. The hotel site is written again with `root /var/www/Octane/dist` (also when it came from an older version of
   the web access step), and the step checks through Traefik that
   `index.html` and `configuration/renderer-config.json` are served.

6. **Camera and badge imager**: in the emulator's `emulator_settings` (the emulator is stopped while they are written, then started
   again; nothing happens when they are already set):

   | Key | Value |
   |---|---|
   | `camera.url` | `https://example.com/gamedata/camera/` |
   | `imager.location.output.camera` | `/var/www/gamedata/camera` |
   | `imager.location.output.thumbnail` | `/var/www/gamedata/camera/thumb/` |
   | `camera.render.mode` | `auto` |
   | `imager.location.output.badges` | `/var/www/gamedata/c_images/album1584/` |
   | `imager.location.badgeparts` | `/var/www/gamedata/c_images/groupbadge/badgeparts/` |

   The folders (`camera`, `camera/thumb`, `c_images/album1584`, `c_images/groupbadge/badge` and `badgeparts`) are
   created for the `www-data` group.

```sh
nano /var/www/Octane/public/configuration/ui-config.json   # e.g. hotel.name
/var/www/Octane/build.sh                                   # rebuild dist/ with it
```

### Step 12 – Harden the server

Nothing to fill in.

1. **Polaris** (the emulator is stopped while they are written, then started again):
   `ws.whitelist` = only the hotel's address (`example.com`; Polaris drops `www.` itself), the old
   `websockets.whitelist` the same, and `ws.ip.header` = `X-Forwarded-For`.
   That header is right **with Cloudflare too**, with `ws.ip.header.trusted` = Cloudflare's ranges and the same
   ranges for NGINX (see **The players' real IP behind Cloudflare** below).

   **The players' real IP behind Cloudflare.** Cloudflare sends `X-Forwarded-For: <player>`; Traefik trusts that
   only from Cloudflare's ranges and appends the Cloudflare address it saw: `<player>, <Cloudflare>`. Polaris and
   NGINX read the header **from the right** and skip only the proxies they trust (a visitor can write anything on
   the left, so that is the only safe way), so both must trust Cloudflare's ranges, or every player shows up as a
   Cloudflare address (`logged in from IP 172.71.x.x using proxyserver 127.0.0.1`). The installer gives Polaris
   `ws.ip.header.trusted` = Cloudflare's ranges, and NGINX `/etc/nginx/hotel-real-ip.conf` (`set_real_ip_from`
   127.0.0.1 and Cloudflare's ranges, with `real_ip_header X-Forwarded-For` and `real_ip_recursive on` in the hotel
   site). `CF-Connecting-IP` is not used on purpose: Traefik passes it through unchanged, so anyone reaching the
   server without Cloudflare could write their own. Cloudflare changes its ranges now and then, so
   `/var/scripts/CF_Refresh.sh` runs every Sunday at 04:00 from `/etc/cron.d/cf-refresh` (output in
   `/var/log/cf-refresh.log`): it rewrites NGINX's list (a graceful reload), and Traefik's and Polaris' only when the
   ranges really changed (Traefik is restarted, the emulator stopped and started, because it writes its settings
   back when it stops). On an existing server, **Tools → Fix visitor IPs** does all of this at once.
2. **MariaDB over an SSH tunnel**: 3306 stays closed; HeidiSQL and the like use your SSH key. A tunnel arrives
   from 127.0.0.1, which MariaDB counts as `root@localhost` (socket login only), so that account also gets the
   saved root password (`IDENTIFIED VIA unix_socket OR mysql_native_password`). In HeidiSQL: network type
   **MariaDB or MySQL (SSH tunnel)**; on the *SSH tunnel* tab `plink.exe`, the server, port, user and the **`.ppk`**
   (plink only reads PuTTY keys; it cannot ask for the key password inside HeidiSQL, so load the `.ppk` in
   **Pageant** first, and log in once with PuTTY to accept the host key), SSH timeout about 10 seconds; on the
   *Settings* tab host `127.0.0.1`, user `root` or `cms` with the password from
   `configuration/database/<server>/mariadb.txt`.
3. **TCP stack** in `/etc/sysctl.conf` (a `.bak-<time>` copy first; existing lines for these keys are replaced) and
   applied right away: `net.ipv4.tcp_syncookies=1`, `net.ipv4.tcp_max_syn_backlog=2048`,
   `net.ipv4.tcp_synack_retries=3`.
4. **Firewall (ufw)**, from a clean rule set (ufw keeps the old rules as `/etc/ufw/*.rules.<date>`):
   ```
   ufw default deny incoming
   ufw default allow outgoing
   ufw limit <ssh port>/tcp comment 'Rate limit for openssh server'   # every port sshd uses + the one you are on
   ufw allow 80/tcp, 443/tcp                                          # or, with Cloudflare, only from its IP ranges
   ufw --force enable
   ```
   MariaDB (3306) and the emulator's ports (3000, 2096) stay closed: the game runs through Traefik on `/ws`. Traefik runs in Docker
   with the host network, so Docker publishes no ports past ufw. Without IPv6 only Cloudflare's IPv4 ranges are
   used. `/etc/ufw/all-in-1-hardened` marks the firewall as managed: the NGINX, MariaDB, Emulator and Web access
   steps then no longer open ports themselves.
   **Firewall log**: `ufw logging` is switched on (low) when it was off, and in `/etc/rsyslog.d/20-ufw.conf` the last
   line `#& stop` becomes `& stop` (added when missing), so the firewall's messages go only to `/var/log/ufw.log`
   instead of also filling `kern.log` and `syslog`; then rsyslog is restarted. Without rsyslog (Debian 12 keeps only
   the journal) the messages stay in the journal. **Debug → Firewall** shows the log live. A server hardened before
   this counts as not done yet, so the step offers **Apply again**.
5. **Log in again**: a new SSH login through the firewall proves you are not locked out (the current session stays
   open; if it fails, the step says so and `ufw disable` undoes it).

### Step 13 – SSH login: keys only (optional)

After Harden. You choose: **Keep password login** (changes nothing, or allows passwords again), or **Turn password
login off: only SSH keys can log in**. A warning says what that means: lose your key and the only way in is the
console of your hosting provider (KVM / VNC / rescue). You have to tick that you understand it. With keys only, the
step:

1. checks that one of your keys on this computer is in root's `authorized_keys` (the SSH key step puts it there),
2. logs in as root with the key in a separate session (the key password goes in the field): when that fails, nothing changes,
3. writes `/etc/ssh/sshd_config.d/00-all-in-1-keys-only.conf` (`PasswordAuthentication no`, `KbdInteractiveAuthentication no`,
   `PermitRootLogin prohibit-password`); it makes sure `sshd_config` includes `sshd_config.d`, checks with `sshd -t`, reloads sshd
   (open sessions stay connected) and checks the effective values with `sshd -T`,
4. logs in with the key again; when that fails, password login is put back automatically. A password login is tried
   as well, and should now be refused.

To allow passwords again: run the step with **Keep password login**, or delete that file on the server console and
reload ssh.

![SSH login step](public/docs/screenshots/ssh-login-step.png)

### Completed – upload the assets

After the last step (done or skipped) the wizard shows **Successfully completed** (or *Completed, with steps to look
at* when a step was skipped or failed), with what every step did, and then:

1. Download all assets with **Assets → Habbo assets** and send them with **Assets → Deploy Assets**.
2. Or upload them yourself with **WinSCP**: SFTP, the server and SSH port, user `root`, the `.ppk` key from the configuration
   folder (*Advanced… → SSH → Authentication*), into `/var/www/gamedata/`.
3. Where everything goes:

   | Folder in `/var/www/gamedata` | Contents |
   |---|---|
   | `config/` | FurnitureData.json, ProductData.json, FigureData.json, FigureMap.json, EffectMap.json, HabboAvatarActions.json, ExternalTexts.json, UITexts.jsonc |
   | `furniture/` | furniture bundles (&lt;name&gt;.hab) |
   | `clothes/` | clothing bundles (&lt;name&gt;.hab) |
   | `effect/` | avatar effect bundles (&lt;name&gt;.hab) |
   | `pets/` | pet bundles (&lt;name&gt;.hab) |
   | `bundled/generic/` | generic bundles (&lt;name&gt;.hab) |
   | `room/` | room bundles (room/&lt;name&gt;/&lt;name&gt;.json) |
   | `icons/` | furniture icons (&lt;name&gt;_icon.png) |
   | `images/` | loading_icon.png, clear_icon.png, big_arrow.png, reception/ (hotel view), wallet/ (currency icons) |
   | `sounds/` | sounds (&lt;sample&gt;.mp3, sound_machine_sample_&lt;n&gt;.mp3) |
   | `c_images/album1584/` | badges (&lt;code&gt;.gif); custom badges are added here by the emulator |
   | `c_images/catalogue/` | catalogue headers and icons |
   | `c_images/reception/` | login screen images |
   | `c_images/notifications/, Quests/, fx_icons/, habboway/, web_promo_small/, album1358/` | notification, achievement, effect and promo images |
   | `c_images/groupbadge/badgeparts/` | group badge parts (badgepart_&lt;part&gt;.png) |
   | `habbopages/` | the hotel's help/info pages |
   | `custom/` | your own extra files (served as /custom/) |
   | `news/news.json` | login screen news (the welcome message is already there) |
   | `camera/, c_images/groupbadge/badge/` | filled by the emulator: do not upload |

4. After uploading: `sudo /usr/local/sbin/gamedata-permissions` (write access for the emulator), open the hotel
   (**Open &lt;url&gt;** button), register the first account, log out and make it administrator:
   ``UPDATE users SET `rank` = 7 WHERE username = 'YourName';`` (HeidiSQL over the SSH tunnel).

### Running the installer again

Every install step first checks the server and shows what it found:

| Step | Counts as already installed when | Buttons |
|------|----------------------------------|---------|
| SSH key | a public key saved in `configuration/keys/<server>/` is in `/root/.ssh/authorized_keys` | **Skip (already installed)** · Install another key |
| NGINX | NGINX from nginx.org is installed and `nginx.conf` includes `sites-enabled` | **Skip (already installed)** · Reinstall / upgrade |
| PHP | all packages installed, PHP ≥ 8.5.12 and PHP-FPM running | **Skip (already installed)** · Reinstall / upgrade |
| MariaDB | 12.3 from mariadb.org running on 0.0.0.0, users and `habbo` exist, and the saved cms password works | **Skip (already installed)** · Reinstall / upgrade |
| Node.js | Node.js 26 from NodeSource, Yarn and Composer in `/usr/bin` | **Skip (already installed)** · Reinstall / upgrade |
| Polaris / Octane / Renderer | the folder is a clone of that repository, on the branch picked under **Branch** | **Skip (already installed)** · Update (git pull) |
| Emulator | JDK 25, the `emulator` account, a built JAR, `config.ini`, and the emulator running | **Skip (already installed)** · Rebuild and restart |
| Web access | Docker from download.docker.com running, the Traefik container running, the hotel site on 127.0.0.1:85 | **Skip (already installed)** · Set up again |
| Harden | ufw active with deny incoming and no 3306 rule, the TCP settings in `/etc/sysctl.conf`, `ws.whitelist` = the hotel's address and `ws.ip.header` = `X-Forwarded-For`; with Cloudflare also `ws.ip.header.trusted` and NGINX's real-IP file with Cloudflare's ranges | **Skip (already installed)** · Apply again |
| SSH login | sshd -T says no password and no keyboard-interactive login | **Skip (already installed)** · Apply again |
| Octane | `dist/` built and served, `/var/www/gamedata` exists, `socket.url` matches the web access address, the camera URLs are set (ui-config and emulator) and `plainConfigBaseUrl` matches the layout (`/configuration/`, or `/client/configuration/` with AtomCMS) | **Skip (already installed)** · Configure and rebuild |
| AtomCMS (addon) | `vendor/` and the atom theme are built, AtomCMS' tables exist, the hotel site's root is `/var/www/atomcms/public` and `hotel.env` has `CMS=atom` | **Skip (already installed)** · Update and rebuild |

When a step is not done yet, the install button is the highlighted one and **Skip** is next to it
(for example, to keep using the password instead of a key). Skipping changes nothing on the server.
Skipping a step moves on to the next one, and a failed step can be skipped too.

A new step implements `InstallStep` (`source/crates/app/src/gui/step.rs`) and is added to `views::server_steps()`. An addon is a
`Wizard` with its own step list and finish page (and `key_step: false`, see `views/gitea.rs`), plus a sidebar entry in `gui/mod.rs`.

## Addons

**Addons** (in the sidebar under SERVER) opens a menu with one button per addon, for things that go on top of a
finished server installation. In the Hotel look every button (the addons, the dashboard and Tools) is filled with its
own colour (a soft gradient with a few light bubbles), with white text on it; every colour is dark enough for white
text, which a test checks. The Professional look keeps the plain tiles.
The addons and Tools use the login from the start of the program: there is no separate login per page. **Back**
returns to the dashboard and keeps the login (and the SQL Editor's database connection) open.

### CMS

**Addons → CMS** first asks **Would you like another CMS?**: **AtomCMS** (set up completely, below), **EpicNext
CMS** (set up completely, see [EpicNext CMS](#epicnext-cms)), **My own CMS** (from a GitHub link or a ZIP file, see
[Your own CMS](#your-own-cms)) or **Remove the CMS** (see [Removing the CMS](#removing-the-cms)). They work the same
way for the hotel: the CMS is the website and Octane moves to **/client**, and all need a finished server installation. One CMS is the
website at a time; switching keeps the other one's files.

**Ports.** Every CMS gets its own NGINX site (`/etc/nginx/sites-available/cms-<name>`) on 127.0.0.1, on the first
free port from **85 to 95** (85 is the hotel site, so usually 86); a CMS keeps its port when the addon runs again.
Traefik sends `/ws` and Polaris' own `/api` paths to Polaris, `/client`, `/gamedata` and `/custom` to the hotel site
on port 85, and the rest of the address to the CMS. `/opt/traefik/hotel.env` remembers the CMS (`CMS`, `CMS_NAME`,
`CMS_PORT`), so the Web access and Octane steps keep this layout when they run again. The CMS' log lines go to the
hotel site's logs, so **Debug → NGINX** shows them. An AtomCMS set up before CMSes had their own port keeps working
as it was (served by the hotel site); running the AtomCMS addon again moves it to its own port.

### AtomCMS

[AtomCMS](https://github.com/ObjectRetros/atomcms) becomes the hotel's website: visitors register and
log in on the CMS, and the CMS opens Octane on **/client** with an SSO ticket. This automates
[NGINX_Atom_setup.md](https://github.com/duckietm/Complete-Retro-on-Ubuntu/blob/main/NGINX_Atom_setup.md),
adapted to this installer's layout. Run the full server installation first, from the same computer
(the database login comes from `configuration/database/<server>/mariadb.txt`).

![AtomCMS addon](public/docs/screenshots/atomcms-step.png)

It asks for the hotel's name, then:

1. **Clone or update** `https://github.com/ObjectRetros/atomcms` in `/var/www/atomcms` (git pull on a re-run).
2. **.env** from `.env.example` (the old one is kept as `.env.bak-<time>`): `APP_NAME`, `APP_ENV=production`,
   `APP_DEBUG=false`, `APP_URL`, MariaDB on 127.0.0.1 with the `cms` account, `FORCE_HTTPS` and
   `SESSION_SECURE_COOKIE` for https, `RCON_PORT` from the emulator's `config.ini`.
3. **composer install** (with the dev packages: AtomCMS' seeder uses Faker on an empty `users` table).
4. **Database**: `php artisan key:generate` (once), `storage:link`, `migrate --seed` on the first install
   (only `migrate` later). AtomCMS handles the emulator's own `password_resets` table itself, and only adds
   columns with defaults to `users` and `camera_web`. On the first install the website settings are set for
   this server:

   | Setting | Value |
   |---|---|
   | `theme` | `atom` |
   | `nitro_path` | `/client` |
   | `nitro_external_texts_file` | `/var/www/gamedata/config/ExternalTexts.json` |
   | `badges_path` / `badge_path_filesystem` | `/gamedata/c_images/album1584` / `/var/www/gamedata/c_images/album1584` |
   | `group_badge_path` | `/gamedata/c_images/groupbadge/badge` |
   | `furniture_icons_path` | `/gamedata/icons` |
   | `ads_picture_path` / `ads_path_filesystem` | `/gamedata/custom` / `/var/www/gamedata/custom` |
   | `rcon_ip` / `rcon_port` | `127.0.0.1` / the emulator's `rcon.port` |

5. **Theme**: `npm ci` and `npm run build:atom` (the atom theme and the housekeeping).
6. **Octane on /client**: in `package.json` (the change from the guide)

   ```json
   "start": "vite --base=/client/ --host",
   "build": "vite --base=/client/ build && node scripts/minify-dist.mjs",
   ```

   `client-mode.json` gets `plainConfigBaseUrl` = `<url>/client/configuration/` (`plainGamedataBaseUrl` stays
   `<url>/gamedata/`, `apiBaseUrl` stays `<url>`, through Traefik), `renderer-config.json` gets
   `"login.screen.enabled": false` and `"habbicons.asset.root": "/client/habbicons"`, and Octane is rebuilt. `build.sh` puts the `--base` back after a git pull,
   and the Octane clone step undoes only that change before it pulls.
7. **NGINX and Traefik**: AtomCMS' own NGINX site serves `/var/www/atomcms/public` (PHP 8.5 through PHP-FPM) on a
   free port (see Ports above); the hotel site keeps Octane's `dist/` on `/client/` and `/gamedata/`. AtomCMS has an
   `/api` of its own, so Traefik sends only Polaris' paths to the emulator: `/ws`, `/nitro-sec` and
   `/api/{auth,authx,badges,cms,emustats,health,housekeeping,maintenance,public,wired}`.
   `/opt/traefik/hotel.env` gets `CMS=atom` and the port.
8. **Test** through Traefik: the CMS, `/client/`, AtomCMS' `/api/online-count` and Polaris' `/api/health`.
9. **Installation key**: AtomCMS makes the key for its web installer on the first visit, so the step opens
   `/installation` and reads the key from `website_installation` (adding one itself if there is none). The key
   is printed in the log, and shown under the result with a **Copy** button.

The last page shows the installation key with a **Copy** button, to paste in **AtomCMS' web installer**, and the
settings above, which the installer already has filled in. Register your account on the CMS and make it
administrator (``UPDATE users SET `rank` = 7 WHERE username = 'YourName';``); the housekeeping is on `/housekeeping`.
Captcha keys (Turnstile / reCAPTCHA) go in `/var/www/atomcms/.env`. To update AtomCMS later, run the addon again.

### EpicNext CMS

**Addons → CMS → EpicNext CMS** makes [EpicNext CMS](https://gitea.epicnabbo.nl/Remco/EpicNext-Cms) (Next.js, no PHP)
the hotel's website, like AtomCMS. It runs on the server itself, **not in Docker**. It asks for the hotel's name and where to get EpicNext:
a repository link, a branch (empty: the default branch) and the folder in the repository with EpicNext's
`package.json`. By default that is EpicNext's own repository (`https://gitea.epicnabbo.nl/Remco/EpicNext-Cms.git`),
its top folder. For a folder, the step keeps a sparse checkout of just that folder in
`/var/www/epicnext-src` and copies it to `/var/www/epicnext`, keeping `.env`, `node_modules`, the build, `storage` and
imported assets; with an empty folder it clones the repository into `/var/www/epicnext` itself. Then:

![EpicNext CMS](public/docs/screenshots/epicnext-step.png)

1. **Get or update** EpicNext in `/var/www/epicnext` (see above; a re-run fetches the newest commit).
2. **Node.js and pnpm**: EpicNext needs Node.js 26.12.1 or newer (an older Node.js 26 from the Dev tools step is
   upgraded with apt) and the pnpm version its `package.json` pins, through corepack.
3. **.env** from `.env.example` (the old one is kept as `.env.bak-<time>`): `DATABASE_URL` to the hotel database
   (EpicNext reads the emulator's tables) with the first login that works: the `cms` login saved on this computer,
   the one already in `.env`, AtomCMS' `cms` login, or else a MariaDB account of its own, `epicnext` (made with root,
   with the cms account's rights on the hotel database only). `REDIS_URL` is left unset when no Valkey/Redis runs, and empty settings from `.env.example` (such as
   `DISCORD_WEBHOOK_URL=`) are commented out: EpicNext's settings check refuses empty values. Before the migrations
   the step runs that check (`src/env.ts`) itself, so a wrong setting shows in seconds instead of after the build. `HOTEL_NAME`, `APP_URL`, `NEXT_PUBLIC_APP_URL` and `AUTH_URL` (the site's
   address), `AUTH_TRUST_HOST=true` (behind Traefik), a random `AUTH_SECRET` (made once, so logins stay valid), and
   `RCON_HOST`/`RCON_PORT` from the emulator's `config.ini`.
4. **Build**: `pnpm install --frozen-lockfile` (when EpicNext's lockfile pins versions younger than pnpm's minimum
   release age, such as a Next.js release of yesterday, the step names them and when the newest was published: run it
   again once they are old enough; the check is never turned off), the base tables (EpicNext was built on an AtomCMS database and its migrations
   expect AtomCMS' tables, such as `website_housekeeping_permissions`: the ones the hotel database lacks are made
   from its own `drizzle/drafts/0000_premium_spirit.sql`, the emulator's tables are not touched),
   `users.password` widened to `VARCHAR(255)` when it is shorter (Polaris makes it `VARCHAR(64)`, too short for
   EpicNext's Argon2id hashes; the emulator never reads it), `pnpm db:migrate` (only EpicNext's own tables: `website_*`, `radio_*`, `acl_*`
   ...; the emulator's stay as they are) and `pnpm build`. The build runs under EpicNext's own memory cap
   (`scripts/with-memory-cap.sh`, with 1 GB left for MariaDB, the emulator and NGINX) and needs about 3 GB of free
   memory. The CMS is stopped during the build. It also needs disk space: the step checks for 3 GB free before
   `pnpm install` and 5 GB before the build (after removing the old build), and else stops with what uses the disk.
   The build cache is removed again when under 3 GB stays free.
5. **Octane on /client**, as for AtomCMS.
6. **Service, NGINX and Traefik**: the systemd service `epicnext-cms` runs `next start` as `www-data` on
   `127.0.0.1:3002` and starts at boot. EpicNext's NGINX site (see Ports above) passes every request to it,
   websockets and Server-Sent Events (the radio) included. EpicNext has a page of its own on `/client` that opens
   Octane, so Traefik sends only `/client/` (with the slash) to the hotel site. `hotel.env` gets `CMS=epicnext`.
7. **Test** through Traefik: EpicNext on `/` and `/client`, Octane on `/client/` and Polaris' `/api/health`.

Then register on `/register` (or log in with an account the hotel already has), make it admin in the SQL Editor
(`UPDATE users SET rank = 7 WHERE username = 'yourname';`). The step sets `nitro_client_url` (`/admin` → **CMS
Settings**) to `/client/index.html` and `gamedata_root` to the hotel's gamedata folder (`/var/www/gamedata`) when they
are empty, so EpicNext's `/client` page opens Octane on this site with the
player's SSO ticket (`/client/index.html?sso=<ticket>`). More settings (mail, Valkey, Turnstile, OAuth) go in `/var/www/epicnext/.env`, followed by
`systemctl restart epicnext-cms`; its log is `journalctl -u epicnext-cms`. To update EpicNext, run the addon again.

EpicNext and AtomCMS both use `website_*` tables. On a hotel database where AtomCMS was installed, tables with the
same name keep AtomCMS' layout; the step warns about that. Choosing another CMS later stops and disables
`epicnext-cms`; its files stay.

### Your own CMS

**Addons → CMS → My own CMS** makes your own CMS (or any website) the hotel's website, the same way as AtomCMS: the
CMS on the site, Octane on **/client** (with `--base=/client/`, `/client/configuration/` and its login screen off,
because the CMS logs in), and only Polaris' own `/api` paths to the emulator. It asks:

- **Where it comes from**: a **GitHub link** (`https://github.com/owner/cms.git`; the server clones it, and a later
  run updates it with `git pull`) or a **ZIP file** from this computer. A private repository cannot be cloned: the
  step says so instead of waiting for a password; download it as a ZIP then. A ZIP with one folder at the top (like
  GitHub's **Download ZIP**) is unpacked without that extra folder.
- **Folder name**: the CMS goes to `/var/www/<name>`. Lower case letters and digits only, at most 12 (from a GitHub
  link it is filled in with the repository's name). Names of the hotel's own folders are refused.
- **PHP or static**: PHP gets PHP 8.5 through PHP-FPM, like AtomCMS; a static website (HTML, CSS, JavaScript) is
  served as files.
- **Root folder**: the folder inside the CMS with the start page (`index.php` / `index.html`), which visitors'
  browsers get; everything outside it (such as the settings with passwords) stays hidden. Laravel-based CMSes use
  `/public`; when the start page is in the top folder, enter `/`. A root that does not exist is refused, with the
  folders that are there.

A folder that already has other content is moved aside (`<folder>.bak-<date>`, nothing is deleted) when you choose to.
The folder belongs to `www-data`. Then Octane is rebuilt for `/client`, the CMS gets its own NGINX site (see Ports
above) and Traefik its route, and the CMS, `/client/` and Polaris' `/api/health` are tested through Traefik (an error
page of the CMS is fine at this point: its database is not set yet).

**The database is yours to set up**: the installer cannot know the settings of a CMS it does not know. The last page
shows the login to use: the **cms** account the server installation made (host 127.0.0.1, port 3306, the hotel
database, the user and its password, from `configuration/database/<server>/mariadb.txt`), never root. Packages your
CMS needs (`composer install`, `npm run build`) run in **Tools → SSH terminal**, in the CMS folder.

![Your own CMS](public/docs/screenshots/custom-cms-step.png)

### Removing the CMS

**Addons → CMS → Remove the CMS** sets the hotel back to how the server installation left it:

1. **Octane on /** again: `vite` without `--base`, `client-mode.json` with `<url>/configuration/`,
   `habbicons.asset.root` back to `/habbicons`, Octane's own **login screen on** again (with a CMS the CMS logs
   players in, so it was off), and a rebuild. The hotel is unavailable for about a minute while it builds.
2. **NGINX**: the hotel site on 127.0.0.1:85 serves Octane on `/` again, and every CMS site (`cms-*`) is disabled.
   For EpicNext the `epicnext-cms` service is stopped and disabled too.
3. **Traefik**: `/ws` and all of `/api` go to Polaris again, the rest to the hotel site; `hotel.env` gets `CMS=none`.
4. **Test** through Traefik: the site, Polaris' `/api/health` and the websocket.

The CMS' files stay, unless you tick **Also delete the CMS' folder and its NGINX site file** (for EpicNext also its
service file and its git checkout). Its database tables
(such as AtomCMS' `website_*` tables) and the users stay in the hotel database. A CMS again: **Addons → CMS**.

### Gitea

**Addons → Gitea** installs [Gitea](https://gitea.com), a light and easy Git server of your own (repositories,
issues, pull requests: a small GitHub), in Docker behind the hotel's Traefik, following Gitea's
[reverse proxy guide](https://docs.gitea.com/administration/reverse-proxies/). Run the Web access step first.

It asks for:

- **The domain**, a name of its own: for a hotel on `example.com`, `git.example.com` (filled in for you).
- **Cloudflare** yes or no (filled in with the hotel's choice).
- **The admin account**: a name (Gitea keeps `admin` for itself, so `gitadmin` is filled in) and an e-mail address.

> **First add the domain to your DNS as an A record** pointing to the server, and with Cloudflare turn its **proxy on**
> (the orange cloud). Without it Let's Encrypt cannot give Gitea its certificate. The step checks the DNS first: no
> address stops it; Cloudflare chosen but the proxy off (or the other way round) gives a warning.

What it sets up:

- **Docker** in `/opt/gitea` (`compose.yaml`, `.env`, `data/`), image `gitea/gitea:1`, with SQLite: no database to set up.
  Gitea has a Docker network of its own (the image always starts an SSH server inside, which must not meet the
  server's SSH), and only its web port is published, on **127.0.0.1**, on the first free port from 85 to 95 (the
  same check as for a CMS, which also recognises Gitea's own port on a later run).
- **Gitea's SSH is off**: clone and push over https, with your password or an access token. Cloudflare's proxy does
  not pass SSH anyway.
- **Traefik**: `/opt/traefik/dynamic/gitea.yml` routes the domain to Gitea, with a Let's Encrypt certificate when
  the hotel uses https. Behind Cloudflare it drops `X-Real-Ip` (Cloudflare's edge) so Gitea takes the visitor
  from `X-Forwarded-For`; Gitea trusts proxy headers only from 127.0.0.1 and its Docker network.
- **The admin**, once, with a random password (`gitea admin user create --random-password`), shown at the end with a
  **Copy** button and saved on this computer in `configuration/database/<server>/gitea.txt`.
- **No sign-ups**: registration is off and the Register button hidden (`DISABLE_REGISTRATION`,
  `SHOW_REGISTRATION_BUTTON=false`); the admin makes the accounts. The start page is the repository list
  (`LANDING_PAGE=/explore/repos`).
- **Test** through Traefik: waits for the certificate (up to 2 minutes) and checks Gitea's `/api/healthz`.

Running the addon again updates Gitea (`docker compose pull`, `up -d`); the repositories, accounts and the port stay.
Back up `/opt/gitea/data`.

![Gitea addon](public/docs/screenshots/gitea-step.png)

**Removing Gitea**: **Addons → Gitea** asks first: **Install or update Gitea** or **Remove Gitea**. Removing stops
and removes the container and its Docker network (`docker compose down`) and Traefik's route file, so the domain no
longer reaches Gitea and its port is free again; the hotel, Traefik and NGINX keep running. The repositories, accounts
and settings stay in `/opt/gitea` (installing again brings them back, with the same admin), unless you tick **Also
delete the repositories, accounts and settings**: then `/opt/gitea`, the image and the saved admin password
(`gitea.txt`) are deleted too. Remove the domain's A record yourself when you no longer need it.

### Fail2Ban

**Addons → Fail2Ban** installs [fail2ban](https://github.com/fail2ban/fail2ban) to protect SSH against password
guessing: after too many failed logins an address is banned in the firewall for a while. It asks first: **Install or
change Fail2Ban** or **Remove Fail2Ban**.

- **Settings**: failed logins before a ban (5), counted within (10 minutes), ban time (60 minutes), and whether repeat
  offenders are banned longer each time (`bantime.increment`, up to a week).
- **Whitelist** (`ignoreip`): addresses and networks that are never banned, one per line (IPv4, IPv6 or a network such
  as `192.168.1.0/24`; text after `#` is a comment). The address this computer reaches the server from is filled in,
  so a wrong password cannot lock you out. The server itself (`127.0.0.1/8`, `::1`) is always on it.
- **Install**: `apt-get install fail2ban python3-systemd`; the settings go to `/etc/fail2ban/jail.d/all-in-1.local`
  (the distribution's own files stay untouched). The `sshd` jail watches every port sshd listens on and reads the
  systemd journal (Ubuntu 24.04+ and Debian 12+ have no `auth.log` by default); without systemd it reads
  `/var/log/auth.log` (rsyslog is installed for it). `fail2ban-client -t` checks the configuration first: one it
  refuses is not used, the previous one stays. Then fail2ban is enabled at boot, restarted and the jail checked.
- Running the addon again saves new settings; bans stay.
- **Remove Fail2Ban** stops it (every ban is lifted), takes it off at boot, deletes `all-in-1.local` and removes the
  package.

Later, **Tools → Fail2Ban → Whitelist and bans** edits the whitelist (with **Add my IP**) and shows the banned
addresses: tick one to let it in again. Saving restarts fail2ban with the new whitelist; whitelisted addresses that
were banned are let in at once. **Debug → Fail2Ban** shows its log live (failed logins, bans, unbans). Locked out anyway? Log in from another address (or the provider's web console) and run
`fail2ban-client set sshd unbanip <your IP>`.

### CrowdSec

**Addons → CrowdSec** adds [CrowdSec](https://www.crowdsec.net) to the hotel's Traefik: it watches every request and
blocks addresses that attack the site (vulnerability scans, probing, known exploits, brute force). Attacks seen by all
CrowdSec users are shared, so addresses that attacked others are blocked before they reach you. It asks first:
**Install or update CrowdSec** or **Remove CrowdSec**. Run the Web access step first (Docker and Traefik).

What you need: **a free CrowdSec account and its enrollment key**. The step walks you through it:

1. **Make a free CrowdSec account**: **Open app.crowdsec.net** opens the console; sign up (the free Community plan is
   enough) or log in.
2. **Copy your enrollment key**: in the console, **Security Engines → Engines → Add Security Engine** (a new account
   shows it on the welcome page) shows a command like `sudo cscli console enroll -e context cm1abc...`. Paste it in
   the step: the whole command is fine, the installer takes the key out (and says when it found one).
3. **Name of this server in the console** (the hotel's address is filled in), and the **whitelist** (never banned;
   this computer's IP is filled in).
4. After the install: **accept the server in the console** (the finish page's **Open https://app.crowdsec.net**
   button; on the welcome page or under Security Engines, **Accept enroll**).

**Cloudflare** is checked first, because it decides which address is the visitor's:

- The step reads how Web access was set up and looks up the domain: is its address one of Cloudflare's (proxy on)?
- Behind Cloudflare, CrowdSec and the bouncer take the visitor's real address from `X-Forwarded-For`, trusted only from
  Cloudflare's IP ranges (the weekly `CF_Refresh.sh` keeps the bouncer's list current too). Someone who reaches the
  server around Cloudflare cannot fake an address that way.
- When the domain points to Cloudflare but Web access was set up **without** Cloudflare, the step stops: every visitor
  would look like Cloudflare, and a ban would block everyone. Run Web access again with Cloudflare on **Yes** first.
- Without Cloudflare (or on an IP address) visitors connect straight to the server and CrowdSec sees their own address.

What it sets up:

- **The security engine** in Docker (`/opt/crowdsec`, `crowdsecurity/crowdsec:latest`) with the collections
  `crowdsecurity/traefik`, `crowdsecurity/http-cve` and `crowdsecurity/whitelist-good-actors`. It reads Traefik's access
  log (`/var/log/traefik/access.log`, rotated daily, a week kept). Its API listens only on `127.0.0.1` (the first free
  port from 8080). The whitelist becomes CrowdSec's allowlist `all-in-1`.
- **The hotel's own files are never counted**: requests for `/gamedata/`, `/client/`, `/custom/`, `/swf/`,
  `/nitro-assets/`, `/assets/` and `/_next/static/` are skipped (a room loads hundreds of furniture files at once, which
  would otherwise look like a crawler).
- **The console**: `cscli console enroll` with your key and name. A key the console does not take is reported, with
  CrowdSec's own error; nothing is marked as enrolled then. On a rerun the key may stay empty: the enrollment is kept.
- **The bouncer**: Traefik's [CrowdSec plugin](https://github.com/maxlerebourg/crowdsec-bouncer-traefik-plugin)
  (v1.8.1), on both entry points, so on every route (the hotel, the CMS, Polaris, Gitea). A banned address gets
  `403 Forbidden`. It keeps the ban list in memory and fetches it once a minute (no question to CrowdSec per request).
  If CrowdSec stops, the hotel stays reachable (nobody is blocked until it runs again). Its settings are in
  `/opt/traefik/dynamic/crowdsec.yml`.
- **Safely**: Traefik restarts once (players are disconnected for a moment). The step compares the hotel's answer
  through Traefik before and after (status and server), and checks Traefik's log for plugin errors. If anything
  differs, for example because the server cannot reach `plugins.traefik.io` to download the plugin, the previous
  Traefik configuration comes back by itself and the hotel runs as before.
- **Test**: waits until the bouncer has fetched the ban list, and shows how many lines of the access log CrowdSec read.

Running the Web access step again keeps CrowdSec in Traefik's configuration. Your own additions to either Docker
service go in `compose.override.yaml` next to its `compose.yaml` (`/opt/crowdsec`, `/opt/traefik`): the installer uses
it and never overwrites it.

Updating: **Tools → Update CrowdSec** (or **Update** on the dashboard, which checks its image like Traefik's and
Gitea's) downloads the newest image and detection rules; the bans, whitelist and enrollment stay. Running the addon
again also updates Traefik's plugin to the version this installer knows.

Later: **Tools → CrowdSec whitelist and bans** edits the whitelist (Traefik reads it without a restart) and lifts bans;
**Debug → CrowdSec** shows its log live. On the server: `docker exec crowdsec cscli decisions list`,
`cscli decisions delete -i <IP>`, `cscli alerts list`, `cscli metrics`.

**Removing CrowdSec** takes the bouncer out of Traefik first (and restarts it), then stops CrowdSec. `/opt/crowdsec`
(its data and enrollment) stays, so installing again needs no new key, unless you tick **Also delete CrowdSec's data
and enrollment**. Remove the engine from your console yourself when you no longer need it.

### Default catalogue

**Addons → Default catalogue** installs the catalogue that comes with the installer, for a new hotel: the catalogue
pages and items, the furniture list (`items_base`) and the client's `FurnitureData.json`, which belong together. The
step shows the catalogue's export date and what the hotel has now. Run Install New Server first (MariaDB, the
emulator and Octane); the furniture files and catalogue icons come from [Deploy Assets](#deploy-assets).

1. **Check**: the hotel database exists with the emulator's tables, and gamedata exists. When the hotel already has a
   catalogue, a box confirms that it may be replaced.
2. **Upload** `Catalog_Export.sql` and `FurnitureData.json` (packed into the program, about 1.7 MB) to the backup
   folder.
3. **Stop the emulator**, once, for everything below (players are disconnected). Another furniture language is
   downloaded before this, while the hotel still runs, so a failed download changes nothing.
4. **Backup and import**: a safety backup of the database (`<db>-before-catalog-<time>.sql.gz`), the SQL drops and
   recreates the catalogue tables and `items_base` (and sets the catalogue's item ids in `emulator_settings`), and
   `FurnitureData.json` goes to `gamedata/config` (an existing one is kept as `.bak-<time>`). A failed import puts the
   database back.
5. **Furniture language** (see below).
6. **Start the emulator**, once, and wait until it has loaded. It also starts when a step above failed. When it does
   not load with the new catalogue, the database (from the backup of step 4) and the old `FurnitureData.json` go back
   and it starts on them.

**Furniture language**: the catalogue's own `FurnitureData.json` is Dutch. Choose another language with its flag
(English, Español, Português, Deutsch, Français, Italiano, Nederlands, Suomi, Türkçe) and, after the import, the
installer does what the [All-in-1 Converter](https://github.com/duckietm/all-in-1-converter) does:

- downloads that hotel's furnidata on this computer (`https://www.<hotel>/gamedata/furnidata_xml/1`) and converts it to
  `FurnitureData.json` (`Furnidata.cs`, `FurnitureDataModel.cs`);
- keeps the hotel's own furni that the official furnidata does not have, with their own names (so they still show);
- brings the database in line with it (`FixSpriteIDSettings.cs`): `items_base.sprite_id` = the furnidata's id per
  classname, and `catalog_items.item_ids` for the `*_single` wallpapers, floors and landscapes and by public name; a
  bundle that sells several furni (`1;2`) keeps them;
- sets the offer ids (`SetOfferID.cs`): `catalog_items.offer_id` = the furnidata's `offerid` per classname, and a new
  unique one above 60000 (in the furnidata too) where it is `-1`;
- puts `FurnitureData.json` in `gamedata/config` (the old one kept as `.bak-<time>`) and runs the updates in batches of
  100, after a safety backup; when something fails, both go back. In the addon this happens while the emulator is
  stopped for the import (one stop and one start for the whole step); Tools → Furniture language stops and starts it
  itself.

**ExternalTexts.json** (optional): tick **Also ExternalTexts.json (the client's texts)** in the step, or answer
**Furniture and texts** in the tool, and the same hotel's `external_flash_texts` (`https://<hotel>/gamedata/external_flash_texts/1`)
is downloaded and converted like the converter's `Flash_Texts.cs` (`key=value` lines; blank lines and `#` comments
skipped; split at the first `=`). Texts the hotel has but the official file does not stay (the emulator writes the
names of custom badges there, hotels add their own). It goes to `gamedata/config/ExternalTexts.json` while the
emulator is stopped (the old file kept as `.bak-<time>`), and comes back when the emulator does not load afterwards.

**Catalogue page texts**: the captions and texts of the catalogue pages (`caption`, `page_text1`, `page_text2`,
`page_text_details`, `page_text_teaser` of `catalog_pages` and `catalog_pages_bc`) follow the furniture language too.
The translations are dictionaries in `source/crates/app/assets/catalog/texts/<lang>.json`, from the Dutch text as it
is in the catalogue to the text in that language. A text in the database is recognised in any of the languages, so
English → German works as well; a text that is in no dictionary (a page you wrote yourself) is never touched, and a
text without a translation stays as it is. The changes go in one transaction while the emulator is stopped.

Keeping them up to date after a new catalogue export:

```bash
python source/tools/catalog_texts.py --missing   # replace Full_Catalog.zip first; writes texts/<lang>.missing.json
#   fill in the values of the .missing.json files (keep HTML tags, line breaks and %placeholders%)
python source/tools/catalog_texts.py --merge     # into texts/<lang>.json
cargo test -p aio-app                            # checks every translation against its original
```

Switching later, without importing the catalogue again: **Tools → Furniture language** (download first, then one stop
of the emulator, the database, `FurnitureData.json` and optionally `ExternalTexts.json`, and one start).

A marker in `gamedata/config/.all-in-1-catalog` remembers the installed version, so the step shows **Skip (already
installed)** for the same catalogue. Meant for a new hotel: furniture players already own keeps its `items_base`
number, so on a hotel with a different furniture list it would turn into other furniture.

### Habbo assets

**Habbo assets** (sidebar → ASSETS) downloads Habbo's own assets on this computer, the first part of the
[All-in-1 converter](https://github.com/duckietm/all-in-1-converter) moved into the installer. No server connection is
needed. Everything goes into `Habbo_Assets/gamedata` next to the configuration folder (or a folder you choose), laid
out exactly like `/var/www/gamedata`, so **Deploy Assets** copies it as it is. Only `.hab` bundles, JSON and images
are kept: what Habbo still serves as XML or text is converted to the JSON Octane loads, and no `.swf` or XML is
saved. A file that is already there is not downloaded again, and an error page from Habbo's CDN is never saved as an
asset.

Pick the hotel (its language goes into the furnidata, productdata and texts), tick what you want and press
**Download the selected**, or **Download all**. The switch **Download only .hab** / **Download .hab and .swf** (off,
only .hab, by default; remembered) also fetches the .swf of furniture, clothes, effects and pets, into the asset
tools' `Assets_tools/Original_SWF_Files/<type>`, never into gamedata, so never to the server:

| Download | Into `gamedata/` |
|---|---|
| Badges (from all nine hotels) | `c_images/album1584` |
| Clothes | `clothes/*.hab`, `config/FigureData.json`, `config/FigureMap.json` |
| Effects | `effect/*.hab`, `config/EffectMap.json`, `config/HabboAvatarActions.json` |
| Furnidata | `config/FurnitureData.json` |
| Furniture | `furniture/*.hab`, `icons/*_icon.png` |
| Catalogue icons | `c_images/catalogue` |
| MP3 | `sounds` |
| Productdata | `config/ProductData.json` |
| Quest images | `c_images/Quests` |
| Reception images | `c_images/reception`, `c_images/web_promo_small`, ... |
| Texts | `config/ExternalTexts.json` |
| Variables | `downloads/ExternalVariables.json` (stays on this computer) |
| Pets | `pets/*.hab` |
| Effect icons | `c_images/fx_icons` |

Habbo has no download link for the effect icons (`fx_icon_<type>` for the avatar editor): they are inside the Habbo
Classic app, in `habbo-inventory-com.hab` inside its `resources/app.asar`. Choose that `app.asar` (choosing it ticks
**Effect icons**); **Download all** takes them along when it is set. **Clothes: make every clothing set wearable** turns off `sellable` in
FigureData.json, the converter's "disable all sellable" question. The `.hab` files are saved as Habbo serves them
(the converter's optional WebP repacking of their sheets is not part of this yet).

### Nitro retro (custom assets)

**Nitro retro** (sidebar → ASSETS) downloads a Nitro hotel's own furniture and clothes. Fill in the six values from its
`renderer-config.json` (`furnidata.url`, `furni.asset.url`, `furni.asset.icon.url`, `avatar.asset.url`,
`avatar.figuredata.url`, `avatar.figuremap.url`; a folder or the full `.../%libname%.nitro` pattern), give it a
name and **Save**. The three JSON fields also take a file on this computer (**Choose ...**: a FurnitureData.json,
FigureData.json or FigureMap.json saved from the browser, for example); the bundles then still come from the folder
links. Every retro is a **profile**: switch between them with one click, **+ New profile** starts an
empty one, **Delete profile** removes one (its downloaded folder stays). Profiles are kept in Settings, so they are
there next time (also saved when you press **Download**).

- **Custom furniture**: the retro's FurnitureData.json, every furniture bundle and the icon of every colour. An old
  Flash retro's (or Habbo's) furnidata XML works too: it is saved as FurnitureData.json, and its revision per furni
  is used for hof_furni-style folders (`<folder>/<revision>/<name>.swf`, then `<folder>/<name>.swf`).
- **Custom clothes**: FigureData.json, FigureMap.json and every clothing bundle (not hh_human_fx and hh_pets);
  figuredata and figuremap XML are converted to JSON.

Each profile downloads into `Habbo_Assets/custom/<name>/gamedata` (laid out like gamedata; characters a folder name
cannot have become `_`), which Deploy Assets does **not** copy: upload
what you want by hand with the Asset Explorer. A retro serves `.hab`, `.nitro` or `.swf`: choose it per profile (**Format**), or leave it on **Auto**, which finds
out per bundle (trying the one that worked last first) and saves every bundle as `.hab`: a `.nitro` is repacked
with the same files, a `.swf` is converted (furniture, clothes, pets and effects: the All-in-1 converter's SWF
compilers ported to Rust, the JSON and sprite sheet as it makes them; the sheet stays PNG). A `.swf` that cannot be
converted is kept in `custom/<name>/source/`; **Convert the kept .swf files** tries those again without downloading. The bundle formats and conversions live in their own crate, `aio-assets`, so other parts of the
installer can use them too.

### Asset tools

**Asset tools** (sidebar → ASSETS) works on files on this computer, in `Habbo_Assets/Assets_tools`, the All-in-1
converter's Hotel Tools for bundles with one clear folder layout (the folders are made for you):

```text
Assets_tools/
  Original_SWF_Files/<type>/       put .swf files here (type: furni, clothes, pets, effects)
  Original_NITRO_Files/<type>/     put .nitro files here
  converted/<type>/                the .hab files SWF / NITRO to HAB makes
  Decompiled_SWF_Files/<type>/<name>/   debugging: images/, binaryData/ (the XML), symbolClass/symbols.csv
  Decompiled_HAB_Files/<type>/<name>/   each .hab as its .json and image, to edit
  Compiled_HAB_Files/<type>/       the edited folders packed as .hab again
```

- **Decompile SWF files**: Original_SWF_Files → Decompiled_SWF_Files, for debugging (images, XML and symbols, like
  FFDEC's export; no ActionScript or sounds).
- **Decompile HAB files**: converted → Decompiled_HAB_Files; **Compile HAB files**: Decompiled_HAB_Files →
  Compiled_HAB_Files (every file in the folder goes in, as edited).
- **SWF / NITRO furniture, clothes, pets, effects to HAB**: Original_SWF_Files/<type> or Original_NITRO_Files/<type>
  → converted/<type>. The .swf can also come from a Nitro retro profile (**Custom: <name>**: the .swf it kept in
  `custom/<name>/source/`). A .hab already in converted is skipped unless **Replace** is ticked. The work runs on
  all CPU cores but one; one broken file fails alone.

### Deploy Assets

**Deploy Assets** (sidebar → ASSETS) copies what **Habbo assets** downloaded to `/var/www/gamedata` on the server, where Octane loads it,
folder for folder (`gamedata/furniture` to `/var/www/gamedata/furniture`, and so on): the download already has the
server's layout.

![Deploy Assets](public/docs/screenshots/deploy-assets.png)

The step compares every folder with the server (name, size and date) and lists what is new or changed; untick a folder
to leave it out. Only those files are sent, per folder as one tar stream over the SSH connection (no FTP, no temporary
files), and they keep their dates, so the next sync sends only what was downloaded since. Nothing on the server is
deleted: your own furniture, badges and the emulator's files stay. Before uploading it checks the free disk space;
afterwards `gamedata-permissions` gives the emulator and NGINX access again.

The config JSON files are only added when they are missing: hotels and the emulator edit them (custom furniture,
badge texts). Tick **Also replace config JSON files** to overwrite them with the downloaded ones. New furniture also
needs its rows in `items_base` and `catalog_items`: **Import Assets** adds them (**Import custom furniture** for a
retro's own furni).

#### UI texts

The second step of Deploy Assets installs the client's own interface texts (buttons, windows, messages). Octane's
`renderer-config.json` loads them from `${gamedata.url}/config/UITexts.jsonc`, and Octane ships them per language in
[public/configuration](https://github.com/duckietm/Octane/tree/main/public/configuration) as
`UITexts_<language>.jsonc.example` (ar, de, en, es, fr, it, nl).

Choose the language; the step copies `UITexts_<language>.jsonc.example` from the Octane clone on the server
(`/var/www/Octane/public/configuration`), or downloads it from GitHub when there is no clone, to
`/var/www/gamedata/config/UITexts.jsonc` (owner www-data). It shows which language is installed now. A
`UITexts.jsonc` that matches none of Octane's languages (your own edits) is kept as `UITexts.jsonc.bak-<time>` first.
Skip either step to run only the other one.

![Deploy Assets: UI texts](public/docs/screenshots/deploy-ui-texts.png)

#### Images (emulator assets)

The third step puts the client images and group badge parts that the Habbo downloads do not have into gamedata. Polaris
ships them in its repository (`/var/www/emulator/Assets`), and the installer has a copy in `Assets/` next to its
executable (from `source/Assets/` in this repository; put extra images of your own there too):

| `Assets/...` | Server (`/var/www/gamedata/...`) | Used by |
|---|---|---|
| `images` | `images` | Octane: the preload images `loading_icon.png`, `clear_icon.png`, `big_arrow.png`, the wallet icons, navigator and catalogue extras |
| `images/reception/*.png` | `c_images/reception` | Octane's hotel view (`stretch_blue`, `sun`, `drape`, `ts`, `US_right`, `US_top_right` in ui-config) |
| `badgeparts/badgepart_*.png` | `c_images/groupbadge/badgeparts` | group badges: Octane (`badge.asset.grouparts.url`) and the emulator's badge imager (`imager.location.badgeparts`) |
| `c_images/...` (talent, habboway, snowstorm_client) | `c_images/...` | Octane's image library (`image.library.url`): talent track, Habbo Way, SnowStorm |
| any other folder `X` in the installer's `Assets/` | `X` | added to the installer's `Assets/` = uploaded to the same path, no code change needed |

Files the Polaris clone has are copied on the server itself; the rest is uploaded from the installer. Only missing files
are added, so images you changed stay. Without the preload images Octane does not start: it drops the SSO ticket it got
from the CMS and logs in with an empty one, and the emulator logs `Client is trying to connect with an invalid SSO
ticket!` for every login (the browser console shows the file: `Initialization failed — falling back to login`). The
Octane step of Install New Server does this too, and Tools → Update emulator after every pull; run this step for a
server installed before that.

![Deploy Assets: emulator images](public/docs/screenshots/deploy-emulator-assets.png)

### Asset Explorer

A two-pane file manager in the style of Norton Commander, for everything in `/var/www/gamedata`. It uses the login from the
start of the program; the page shows **this computer** on the left (the drives C:, D: … on
Windows, `/` and your home folder on Linux) and the **server** on the right, **locked to `/var/www/gamedata`** and the
folders in it: there is no way above it.

![Asset Explorer](public/docs/screenshots/asset-explorer.png)

| Key | |
|---|---|
| **Tab** | switch between the panes (the active one has a coloured border) |
| **Enter** / double-click | open a folder (`..` goes up) |
| **Backspace** | up one folder (on the server not above gamedata) |
| **F5** | copy the selection to the other pane: **Upload →** from the left, **← Download** from the right; folders with everything in them |
| **F6** / F2 | rename |
| **F7** | new folder |
| **F8** / Delete | delete (asks first) |
| **Ctrl+R** | refresh both panes |
| Ctrl/Shift+click, Ctrl+A | select more files |
| **Drag and drop** | drag files or folders to the other pane (upload or download); drop on a folder row to copy into that folder. Files and folders from Windows Explorer / your desktop can be dropped on the server pane to upload them, and files from the left pane can be dragged out to Explorer or the desktop |

- **Rights:** everything that is uploaded or created on the server gets owner `www-data:www-data`, folders `2775`
  and files `664`, so NGINX serves it and the emulator (group www-data) can change it.
- **Already there:** when a file or folder exists on the other side, it asks: replace, keep the existing files (copy
  only what is missing), or cancel.
- **Preview:** selecting an image (png, gif, jpg …) on either side shows it with its size in pixels.
- **Locked to gamedata:** every server action checks the path twice: the installer refuses `..` and separators in
  names, and the script on the server resolves the real path (`realpath`, which follows symlinks) and stops when it
  is not in `/var/www/gamedata`, so a symlink in gamedata that points elsewhere cannot be opened, written or deleted
  through it. gamedata itself cannot be renamed or deleted.
- Uploads go as one tar stream over the SSH connection (like Deploy Assets), downloads over SFTP; the progress and
  speed are shown under the panes.
- Large selections are fine: dropping 10 000 files into `c_images` takes seconds, and so do deleting them or reading
  them for a download. The rights are set in batches from `tar`'s own list, and long selections are split into
  scripts under Linux's 128 KB limit per command argument.

### SQL Editor

A database client in the style of HeidiSQL, inside the installer. It uses the login from the start of the program and
reaches MariaDB through that SSH session: a tunnel from a free port on `127.0.0.1` of this computer
to `127.0.0.1:3306` on the server, so the MariaDB port does not have to be open (the Harden step closes it).

It logs in to MariaDB with the root account from `configuration/database/<server>/mariadb.txt` (written when this
computer installed MariaDB). Without that file, or when MariaDB refuses the password, it asks for a user, password and
port; **Log in to MariaDB** asks again.

- **Left:** the databases; open one to see its tables and views with their size. The hotel database is opened first.
- **Data:** the rows of the selected table, 1000 at a time (**Previous** / **Next**). Type a `WHERE` condition in the
  filter (`id > 100 AND username LIKE 'a%'`) and press Enter. Clicking a column header sorts the whole table on the
  server. In a table with a primary key, **double-click a cell** to change it (Enter saves at once, Esc cancels);
  right-click for **Set to NULL**, copy the value or the rows, and **Delete selected rows** (asks first). Tables
  without a primary key and views are read-only.
- **Structure:** the columns (`SHOW FULL COLUMNS`) and the `CREATE TABLE` statement.
- **Query:** an editor for SQL. **F9** or **Ctrl+Enter** runs the selected text, or everything when nothing is
  selected. Statements run one by one in the database chosen above the editor (`USE` changes it), like the mariadb
  client: `DELIMITER` works for procedures and triggers, and the first error stops the script and selects its line.
  Every result set gets its own tab (at most 10,000 rows; add a `LIMIT` for others), and **Messages** shows how many
  statements ran and how many rows they changed. **Stop** cancels a running query on the server (`KILL QUERY`).
  **Open .sql** / **Save .sql** load and save the editor; **Export CSV** saves a result (or the table data) as UTF-8 CSV.

`NULL` is shown as `(NULL)`, binary values as hex. Changes are made at once (autocommit), so make a backup first with
**Tools → Create full database backup** before bigger changes. Big dumps go through **Tools → Import full database**.

### Catalogue

**HOTEL → Catalogue** edits the hotel catalogue in the database the emulator uses (`db.database` in its `config.ini`),
through the same SSH tunnel and MariaDB login as the SQL Editor.

- **Left:** every catalogue page as a tree, with a search on the name, the internal name or the id. Hidden or switched
  off pages are grey.
- **Page:** name, internal name, layout, catalogue (normal, Builders Club or both), parent page (never under itself),
  order, icon, minimum rank, visible, enabled and club only. **Save page** writes it; **Undo changes** goes back.
- **Offers:** the offers of the page with their furni, prices, limited series (sold / size) and order. Click one to
  edit its name, furni ids (one id, or a bundle `100;200`; under the form the offer's furni are listed by name with a
  button to take one out, and **Search** finds furni in `items_base` by name, class name or id to **Add**), credits, points and their type, amount, limited size (never
  below what is sold), order, offer id, page, extra data, club only and bulk buying.
- **New page** makes a page under the page shown (or at the top level), after its sisters and hidden until you tick
  Visible; **New offer** adds one to the page (fill in its furni ids). **Delete page** removes a page with its offers
  and **Delete offer** one offer, after asking. A page with sub-pages, or an offer with sold limited numbers, is
  never deleted: move the sub-pages first, and the record of who has which number stays.
- **Front page** (the second tab) edits the promo tiles of the catalogue front page (`catalog_featured_pages`): the
  slot number, caption, image (a path under the catalogue images, `catalogue/...`), what it opens (a page by internal
  name or id, or a product) and when it stops showing (`YYYY-MM-DD HH:MM` in this computer's time zone; empty never).
  **Save front page** replaces the whole set at once. The table is MyISAM, so a save that fails halfway is not rolled
  back: save again.
- **Limited** (the third tab) lists every limited series: its offer and page, and how many are sold of how many,
  with a search. **Grow** makes a series bigger (the emulator makes the new numbers, after the last one, when it loads
  the catalogue); a series never gets smaller, as its numbers already exist. **No longer limited** ends a series of
  which nothing is sold; **Remove with its sales** ends one that sold, after asking, and deletes its numbers and the
  record of who has which one for good (the furni players have keeps its number).
- **Vouchers** (the fourth tab): the codes players type in the catalogue, with what each gives (credits, points of a
  type, an offer), how often it was used of how many uses (0: without end) and how often one player may use it (-1:
  without end), and a search. **New voucher**, **Save voucher**, **Delete voucher** (with the record of its uses, after
  asking); a code is 1 to 10 letters, digits, `-` or `_` and is never used twice, as the emulator matches it exactly.
  **Make codes** adds up to 500 vouchers with random codes (an optional prefix, then at least 4 easy-to-read letters and
  digits) that each give what the form shows. A voucher's **Used by** lists who used it and when.
- **Checks** (the fifth tab) finds **pages with the same internal name** (a `catalog/open/<name>` link reaches only one
  of them) and **offers that sell the same furni** (the same furni ids in any order, with the same extra data; for `wallpaper_single`, `floor_single` and
  `landscape_single` offers also the same pattern number in the name, as the emulator reads it from there). Click
  a page to open it in the Pages tab; tick copies (or **Tick all but the first of each**) and **Delete ticked** removes
  them after asking. Offers with sold limited numbers are never ticked or deleted.
- **Unoffered** (the sixth tab) lists the floor and wall furni of `items_base` that no offer sells (an id in no
  offer's furni ids), with a search. Tick some (or **Tick all shown**), choose a page and a price, and **Add N offers**
  adds one offer per furni, named after its class name, after the page's other offers.
- **Backups** (the seventh tab): copies of the catalogue on the server, in `<backups>/catalogue` (Settings → server
  folders). **Make backup** dumps `catalog_pages`, `catalog_items`, `catalog_featured_pages`, `vouchers` and the
  Builders Club `catalog_pages_bc` / `catalog_items_bc` (those that exist) with `mariadb-dump` to a `.sql.gz`, with an
  optional name. **Roll back** first makes a `before-rollback` backup of the catalogue as it is, then puts the chosen
  one back; publish afterwards. Sold limited numbers (`catalog_items_limited`) and voucher uses (`voucher_history`) are
  never in a backup, so a rollback never takes back what players own or used. **Delete** removes a backup file.
- **Build page** (above the tree) makes a new page with an offer for every floor and wall furni whose class name
  starts with a prefix (a furni line, like `rare_dragonlamp`), optionally only furni no offer sells yet: name, internal
  name, parent, layout and price, **Find furni** to see them, then **Build page**. The page starts hidden.
- In **Checks**, **Merge into #id** moves the offers, sub-pages and front page tiles of pages that share an internal
  name to the first one, then deletes the others, after asking.
- **Sort by name** (above a page's offers) numbers its offers 1, 2, ... in the order of their names.
- **Publish** asks the running emulator over its RCON port (`rcon.port` in `config.ini`, on 127.0.0.1 of the server) to
  load the catalogue again (`updatecatalog`); the players online are told to refresh it. Saved changes show in the hotel
  after a publish.

Every change is one statement with its values sent separately, in a transaction; nothing typed in becomes SQL text.


### Catalogue explorer

**HOTEL → Catalogue explorer** shows the same catalogue as the game does, in **two windows side by side** (or one:
**One window**): the root pages as tabs, the page tree with its icons on the left (hidden pages grey, switched-off
ones marked *off*), and the page's furni as a grid with an info box for the one selected: a large icon, its name,
class name and offer, and the price in credits, duckets or diamonds (*LTD* marks a limited series, *+* a bundle).
The icons come from the downloaded assets (**Habbo assets** and the Nitro retro profiles), the furni names from
`items_base`.

A **front page** (layout `frontpage`, `frontpage4`, ...) holds promotions, not furni: it is locked here (no drops;
its promotions are edited on the Catalogue page). The icons are drawn as the game does, each image pixel on whole
screen pixels at any display scale, so they stay sharp (twice their size in the info box).

Furni are dragged like files in the Asset Explorer:

- onto the **other window**, or onto a **page in either tree**: they move to that page, at the end;
- onto **another furni**: they go in before it, also on the same page (to change the order);
- with **Ctrl** held: a copy goes there, the original stays.

**Pages** are dragged in the tree like folders: onto the middle of a page to put it under that page, onto its top or
bottom edge to put it before or after it (also to change the order), or onto a tab to put it under that root page.
A page never goes under itself or one of its own pages, and nothing goes into a front page.

The **Without offer** tab (after the root pages) lists the furni no offer sells yet (search on the name, class name or
id): drag them onto a page in the other window, or a page in its tree, and each gets an offer there (named after its
class name, 3 credits; the price is set on the Catalogue page).

**Right-click** a furni (in a page or on the Without offer tab) and **Edit furni** opens its `items_base` row: every
column of the table as this emulator has it, read from the database (so other columns work too). The `enum('0','1')`
flags (allow_sit, allow_trade ...) are ticks, other enums a list (`type`), numbers and text fields with their column
type next to them (the column's comment on hover); a column that may be NULL has a NULL tick, and `id` stays as it
is. Changed values are marked and checked (whole numbers, decimals, the text length); **Save changes** writes only
those, in one statement. A bundle offers an entry per furni. The emulator reads `items_base` when it starts: restart
it (Tools) or use `:update_items` in the hotel.

The same menu has **Edit offer**: the offer's `catalog_items` row in the same editor (price in credits and points
with their type, amount, page, order, limited series, offer id, extra data, the flags as ticks ...). After a save
the page is read again (also the new page when `page_id` changed); **Publish** shows it in the hotel.

**Right-click a page** (in a tree, on its tab, or on the page bar above its furni) for **Edit page**: the page's
`catalog_pages` row in the same editor (caption and internal name, `page_layout` from the layouts the table allows,
icon, parent, order, minimum rank, visible / enabled / club / VIP as ticks, headline and teaser images, the page texts
in fields of several lines ...). After a save the tree is read again; **Publish** shows it in the hotel.
`icon_image`, `page_headline`, `page_teaser` and `page_special` show their image next to the field and have
**Choose ...**: a picker with every page icon (`c_images/catalogue/icon_<n>.png`, by number) or every catalogue image
(the headlines and teasers), from this computer and the server's `gamedata/c_images/catalogue`, with a search. The
server's images are fetched over the SSH connection as they come into view. A click writes the icon's number, or the
image's name without `.gif` (the client adds it).

Ctrl-click and Shift-click select several. Every drop is saved at once in one transaction (the page's offers are
numbered again in their new order); **Undo** puts the last drop back. **Publish** makes the emulator load the
catalogue again, as on the Catalogue page.

### Content

**HOTEL → Content** edits the hotel's plain content tables in the same database, through the same SSH tunnel and
MariaDB login as the Catalogue. Each tab has a list on the left (with a search and **New**) and the row's form on the
right; a changed form says *Not saved yet* until **Save** (or **Add**), **Undo changes** goes back, and **Delete**
asks first. Values are checked before saving (number ranges, text lengths, the table's unique key).

- **Achievements** (`achievements`): name (without `ACH_`: the badge is `ACH_` + name + level, `ACH_RoomEntry1`),
  category, level (each level of a name once), progress needed, reward amount and currency type, score, state
  (0 off, 1 on, 2 archived, 3 off-season, 4 by wired), display method and subcategory. The hotel loads them with
  `:update_achievements` (or a restart of the emulator).
- **Daily tasks** (`daily_tasks`) and **Quests** (`quests`): code (and campaign for quests, unique together), goal
  (pick one of the emulator's goals, or type its enum name), goal data and count, reward, images, order, enabled. The
  emulator reads these only when it starts: **restart the emulator** to load the changes.
- **Polls** (`polls`): title, thank-you message and reward badge, with the poll's **questions** (`polls_questions`)
  under it: order, question, type (1 one answer, 2 several, 3 written), least answers and the options
  `value:label;value:label` (checked, as the emulator fails on an option without `:`). Deleting a poll deletes its
  questions and the answers given (`polls_answers`). The hotel loads them with `:update_polls`.
- **Currencies**: the emulator settings that name the currency types (`seasonal.types`, `seasonal.primary.type`,
  `seasonal.currency.*`, `hotel.marketplace.currency` in `emulator_settings`), each saved on its own, and how many
  players hold each type and how much in all (`users_currency`). The hotel loads them with `:update_config`.
- **Public rooms** (`navigator_publiccats`): the navigator's public categories (name, shown as pictures, visible,
  order: the navigator lists them from the highest order down), with the category's **rooms** (`navigator_publics`)
  under it: room id and visible, each room once per category. Deleting a category removes its room list (the rooms
  stay). **Room categories** (`navigator_flatcats`): caption (the text, or a `${navigator...}` text key), saved
  caption, lowest rank, most users in a room, trading, official, list type (0 list, 1 pictures) and order. A room
  category that rooms still use is not deleted: the page says how many rooms to move first. Both load with
  `:update_navigator`.
- **Pets** (`pet_actions`): pet type, name, offspring type (-1 none) and the happy, tired and random actions, with the
  pet's **breeds** (`pet_breeds`: colours, which colours it uses, club only), **sounds** (`pet_vocals`: when and the
  message; sounds of pet type 0 are said by every pet) and **commands** (`pet_commands`) under it. **Pet commands**
  (`pet_commands_data`): the commands pets can learn, with the level needed, experience and costs. Deleting a pet
  removes its breeds and commands; deleting a command takes it from every pet type. Both load with `:update_pet_data`.
- **Group badges** (`guilds_elements`): the parts of the group badge editor: type (base, symbol, or the base, symbol
  or background colour), number (each type has its own; **New** takes the next free one), image or colour (six hex
  digits for the colours), second image (a symbol in two layers) and enabled. Loaded with `:update_guildparts`, which
  also reloads the badge images.
- **Chat bubbles** (`chat_bubbles`): the hotel's own styles (46 and up): name, permission needed (empty for
  everyone), whether others may override it, whether talking furni react, and the catalogue item that sells it (0 not
  sold). Deleting a style takes it from the players who bought it (`users_chat_styles`). Loaded with
  `:update_chat_bubbles`.
- **Calendar** (`calendar_campaigns`): name, image, start (Unix time), days, lock missed days and enabled, with the
  campaign's **rewards** (`calendar_rewards`) under it: the day (empty: the random pool for the days without their own
  reward; several on one day are picked at random), and what it gives: a furni, credits, duckets, points of a
  currency type, a badge, an item or a subscription (at least one). Deleting a campaign removes its rewards and the
  record of the days opened. Loaded with `:update_calendar`.

Tables that have no id column (`navigator_publics`, `pet_breeds`, `pet_vocals`, `pet_commands`, `pet_commands_data`,
`pet_actions`, `guilds_elements`, `chat_bubbles`) are found by their key values as they were loaded, one row at a time
(`... LIMIT 1`). The reward tracks are not on this page: the hotel edits them itself in the Reward Track window (the
gear tab, permission `acc_rewardtrack`), which reloads them for everyone.

### Logs

**HOTEL → Logs** reads the logs the emulator writes, through the same SSH tunnel and MariaDB login as the Catalogue.
It is read-only: nothing is changed or deleted. Every list shows 200 rows a page, newest first, with **Previous** and
**Next**; player names come from `users` (a removed player shows as `#id`), and times are shown in the time zone of
this computer. A filter can name a player (the exact name: it is looked up first, then the log is searched on the
player's id), a room id where the log has rooms, words (anywhere in the text; `%` and `_` are matched as themselves)
and a date range (**From** and **Until**, `YYYY-MM-DD` with an optional `HH:MM`; a date alone includes that whole
day). Enter in a field or **Search** applies it, **Clear** empties it.

- **Commands** (`commandlogs`): the player, the command, its parameters and whether it worked. Words look in the
  command and the parameters.
- **Room chat** (`chatlogs_room`): the player, who a whisper went to, the room and the message. Click a line to see
  the 10 lines before and after it in the same room, the chosen one marked.
- **Private chat** (`chatlogs_private`): sender, receiver and message; a player filter finds both sides.
- **Currencies** (`logs_economy`): every change of a player's currency: operation, currency type, amount, the balance
  before and after, the item and the context.
- **Purchases** (`logs_shop_purchases`): the catalogue offer bought, how many and what it cost.
- **Permissions** (`permission_audit`): rank and permission changes: who, the action, the target (a player or a
  rank), the subject, the old and new value and how (command or housekeeping). A player filter finds the changes the
  player made and the changes made to them.
- **Staff actions** (`housekeeping_log`): the housekeeping actions of the staff (bans, kicks, room actions) with the
  detail and whether they worked.
- **Room visits** (`room_enter_log`): who entered which room, and when they left (empty while still inside).
- **Name changes** (`namechange_log`) and **Trades** (`room_trade_log`: both players and how many items each gave).

A log table that is not in the database (an older emulator without `logs_economy`, `housekeeping_log` or
`permission_audit`) shows a greyed tab that says so.

### Players

**HOTEL → Players** finds a player and acts on them. It reads through the same SSH tunnel and MariaDB login as the
Catalogue; the changes go to the running emulator over RCON, the way the catalogue is published.

- **Search**: the start of a name (`%` and `_` count as themselves) or an id, 100 players a page, online players
  first and then the ones seen last; **Online only** shows the players in the hotel now.
- **The player**: id, name, rank and its name (`permission_ranks`, or the older `permissions`), motto, look, mail,
  when the account was made and the player was last online, online or not and the room they are in (the room visit
  without a leave time), the home room. The **wallet**: credits, every `users_currency` balance (0 duckets, 5
  diamonds, other types by number) and the club subscription (`users_subscriptions`). The **badges**, the worn ones
  first. The **possessions**: furni by kind (floor, wall, effect, ...) in the inventory and placed in a room, pets and
  bots the same way, and the rooms owned. The last 15 **room visits**, and the **bans** (the active ones in red; an end
  of 0 never ends) and a mute (`users_settings.mute_end_timestamp`). The password hash, the SSO ticket, the remember-me
  token, the secret key and the pincode are never read.
- **Actions (RCON)**: give credits (`givecredits`), give points of a currency type (`givepoints`), give a badge
  (`givebadge`), set the rank (`setrank`), send an alert (`alertuser`) and disconnect (`disconnect`). Each asks first,
  shows the emulator's answer and reads the player again. Credits, points, badges and the rank also work for an
  offline player: the emulator writes them to the database. An alert and a disconnect only reach a player who is
  online. The emulator keeps its own limits: `rcon.grant.max_amount` (default 1,000,000 a grant) and
  `rcon.setrank.max_rank` (default 12) in its config, and it logs a rank change.

### Hotel view

**HOTEL → Hotel view** edits the landing page players see before they enter a room, through the same SSH tunnel and
MariaDB login as the Catalogue. Four tabs:

- **Widgets**: the five slots of `hotelview_landing_slots` (a missing row is shown as a hidden promotion and added when
  saved). Per slot: shown or not, the type (the 23 types the emulator accepts: promotion, bonus rare, catalogue
  promotion, expiring catalogue page, community goal, daily quest, next limited rare countdown, ...), title, text,
  image, button text and link, progress (0 to 100) and its text. **Options of this type** are the keys of
  `config_json` the client reads for that type (position x/y, second button, catalogue page, featured slot, end date,
  badge and goal code, the "use the emulator's LTD / bonus rare / community goal / daily quest" switches); an empty field
  removes its key. Everything else in `config_json` (such as `voteOptions`) is edited as JSON below them. The JSON must
  be an object of at most 3000 characters (the emulator replaces anything else by `{}`); keys you do not touch are kept
  as they were, and the runtime keys `userVote` and `voteCounts` are never saved. A vote slot shows how many votes are
  cast and can **reset** them (`hotelview_landing_votes`).
- **Scene and Hall of Fame** (`hotelview_landing_settings`, row 1): the background, left, right and drape images and
  their positions (-1 is the client's default place), and the Hall of Fame box: shown or not, its place and what it
  lists (newest players, most time online, highest achievement score, or most of one currency type).
- **News** (`hotelview_news`): add, edit and delete articles (title, text, button text, whether the button opens a page
  in the client or a web page, its link, the image). The hotel shows the ten with the highest ids.
- **Bonus rare and goals**: the `emulator_settings` the widgets read: the bonus rare's furni id and class name, the
  points it costs and their currency type, the concurrent users goal and its badge, and how many players the community
  goal Hall of Fame lists. A setting that is not in the table yet is added.

Saving writes the database; the hotel shows it after a reload. **Reload in the hotel as** sends `:update_hotel_view`
over RCON (`executecommand`) as the online staff player you name: the emulator loads the widgets, scene, news and Hall
of Fame again and refreshes the hotel view of everyone online. The player must be online and may use the command
(`cmd_update_hotel_view`); you can also type the command in the hotel yourself. The settings tab is read by the emulator
at start or with `:update_config`.

### Gamedata

**HOTEL → Gamedata** edits two files in the server's `gamedata/config` over the SSH login (no MariaDB password needed):

- **Furnidata** (`FurnitureData.json`): search floor and wall furni by id, classname or name, and edit the plain fields
  of one (name, description, dimensions, offer id, `canstandon`, ...). Each value keeps its type (a number stays a
  number, true/false stays true/false); lists and objects such as `partcolors`, and fields this editor does not know,
  are kept as they are. An id or classname another furni already uses is refused. **Check items_base** looks the
  classname and sprite id up in the hotel's `items_base` (read only, through the root login's MariaDB socket) and says
  whether they match (otherwise run Fix items_base). **Add copy** adds a new furni as a copy of the one shown, with its
  own id and classname and offer id -1 (Fix offer IDs gives it one); it still needs a furni bundle and an `items_base`
  row of that name.
- **Texts** (`ExternalTexts.json`): search a key or a text, edit a text, add a key, delete a key (after a confirm).

Changes go into an edited copy first; **Save** writes the file. Saving checks first that the file on the server is
still the one that was loaded (its SHA-256: an edit by the emulator or the in-hotel furni editor in the meantime is
never overwritten, load it again instead), copies it to `<file>.bak-<time>` next to it, then writes the new file under
a temporary name in the same folder and renames it over the old one, keeping its owner, group and mode. The JSON is
written back in the file's own layout (one line or its indent, a BOM, a closing newline).

There is no reload command: the emulator watches its furnidata (`items.furnidata.watch.enabled`, on by default) and
sends the changed furni to everyone online by itself. Texts are read by the client when it loads, so players see them
after reloading the hotel page. Octane's `UITexts.jsonc` is not edited here, because it has comments that saving as
JSON would drop; FigureData is not edited either, because its set, part and palette ids depend on each other and a
plain field edit easily breaks avatars.

## Settings

**The dashboard's component list** shows what runs on the server, compared with what is newest: the emulator;
**Octane and Octane Renderer** as one row (they depend on each other: both versions are shown, the new commits of both
are counted together, and **Update** updates the renderer first, then Octane, then builds the client); AtomCMS when it is
installed; and the Docker services **Traefik** and, once installed, **Gitea**. For those the check compares the image on
the server with Docker Hub's newest for the same tag (a `HEAD` request of the tag's manifest, which does not count
against Docker Hub's pull limit; `docker buildx imagetools inspect` only when that gives nothing, and to read the newest
version number; nothing is downloaded or changed), shows **NOT CHECKED** with the reason when Docker Hub does not answer
or limits the server's requests, and shows **NEW IMAGE WAITING** when a newer image is downloaded but
the old container still runs (a `docker restart` keeps the image the container was made from; **Update** and
**Restart Traefik** make a new container from it). **Update** runs the matching tool on the Tools page. For Traefik
with a waiting image the button is **Restart** instead: it runs **Restart Traefik**, which makes the new container from
the downloaded image without asking Docker Hub (so it also works while Docker Hub limits downloads), and checks again
afterwards so the row shows the new version.

**Settings** (sidebar, under GENERAL):

- **Look**: Hotel or Professional (see the top of this page).
- **Servers** and **SSH key**: the IP addresses and DNS names of your servers (Add, Remove), with one ticked as the
  default. The login at startup fills in the default and lists the others; it selects the key when you log in with an
  SSH key. Changes are saved right away, and are stored in `configuration/settings.json`, so the encrypted settings backup takes it along.
- **Server layout**: where the hotel lives on the server and who the emulator runs as. The installer installs to these
  places, and the tools, the addons, the Asset Explorer and the dashboard look for everything there:

  | Setting | Default |
  |---|---|
  | Emulator (Polaris clone; it runs from `Emulator/` in it) | `/var/www/emulator` |
  | Emulator account | `emulator` |
  | Emulator logs | `/var/log/emu` |
  | Octane / Octane-Renderer | `/var/www/Octane`, `/var/www/Octane-Renderer` |
  | Gamedata (served on `/gamedata`) | `/var/www/gamedata` |
  | AtomCMS, and **AtomCMS installed**: Detect / Installed / Not installed | `/var/www/atomcms`, Detect |
  | Database backups | `/var/backups/all-in-1` |
  | Hotel database (for new MariaDB passwords files) | `habbo` |

  **Emulator account** is a list of the server's accounts once you are logged in: `root`, the people's accounts and
  the accounts in group `www-data` (where the emulator's account lives), each with a short note. **+ Create a new
  user ...** at the bottom asks for a name and creates it on the server right away as a service account (no login,
  home in `/var/lib/<name>`, member of `www-data`); without a login the name is chosen and the Emulator step creates
  it. An existing account keeps its own home folder for the build's `~/.m2`.
  **Emulator account `root`** works but gets a warning: the emulator, and every plugin in it, could then change
  anything on the server. A normal account (created by the Emulator step) can only touch the hotel's files.
  **AtomCMS installed** decides whether AtomCMS is on the dashboard, in the update check and in **Update AtomCMS**:
  Detect looks for the clone, Installed always lists it (as NOT FOUND when the folder is missing), Not installed leaves it out.
  The values go into scripts that run as root, so only plain absolute paths (letters, digits, `.`, `_`, `-`, `/`),
  Linux user names and simple database names are accepted; system folders such as `/var/www` itself and two parts
  in one folder are refused. A change does not move anything on the server (move the folder yourself, or install
  again) and applies at the next start (**Restart now**). Stored in `configuration/settings.json` under `Layout`.
- **Repository branches**: the branch for Polaris, Octane and Octane-Renderer: **main** or **dev**. This is the
  one choice the whole program follows: the clone steps of Install New Server, **Tools → Update emulator** and
  **Tools → Update Octane**, and the update check that counts new commits. A clone that is on another branch is
  switched over (`git fetch` + `git checkout`) the next time it is updated, never silently: the clone step and the
  update check both say so first. A change made on a clone step is saved back here, so the two always agree.
  Stored in `configuration/settings.json` under `Branches`; a repository left on main is not written.
- **SSH keys**: every key in `configuration/keys`, and **Import a key** for a server: a key file, or paste it. A private
  key (OpenSSH, PEM or PuTTY `.ppk`; it asks for the key's password once to read its public key) is stored as
  `keys/<server>/imported_<type>` with its `.pub`, still protected by its own password. A public key alone is stored as
  `provided_<type>.pub`, for the SSH key step to install for root.

![Settings: servers](public/docs/screenshots/settings-servers.png)

## Debug

**Debug** (sidebar, under TOOLS) follows the server's logs live over the login, newest lines at the bottom:

| Log | What it follows |
|---|---|
| **Emulator** | `<emulator logs>/emulator.log` (Settings → Server layout; `/var/log/emu` by default) |
| **NGINX errors** | `/var/log/nginx/hotel/error.log` and `/var/log/nginx/error.log` |
| **NGINX access** | `/var/log/nginx/hotel/access.log`: every request that reaches the hotel site |
| **Traefik** | `docker logs traefik`: certificates, routing and connection errors |
| **Firewall** | `/var/log/ufw.log` (see the Harden step), or the kernel journal's UFW lines without rsyslog |
| **CrowdSec** | `docker logs crowdsec`: the bans it decides, the hub, the console and the bouncer's connections (see the [CrowdSec](#crowdsec) addon) |
| **Fail2Ban** | `/var/log/fail2ban.log` (see the [Fail2Ban](#fail2ban) addon): failed SSH logins it counted (`Found`), bans (`Ban`) and unbans (`Unban`); the fail2ban journal when there is no log file. Says so when fail2ban is not installed. Type `Ban` in the filter to see only the bans |

It starts with the last lines of the log and then shows what comes in. **Filter** shows only the lines with a text
(for example `BLOCK` or a player's IP), **Pause** holds the view still while lines keep being collected, **Clear**
empties it and **Restart** follows the log again (for example after the connection dropped). The last 5,000 lines are
kept; **Copy log** and Ctrl+C copy them. The log is only followed while the page is on screen: leaving it, or the
connection dropping, stops `tail` / `docker logs` on the server too (the follower runs in a process group of its own,
which is stopped as soon as the SSH channel closes), so nothing keeps running there.

## Tools

The sidebar's **TOOLS** section is for running the hotel. The page shows the server tools, and the settings
backup at the bottom. Without a login the server tools are greyed out, with a **Log in** button above them.

![Tools](public/docs/screenshots/tools.png)

| Tool | What it does |
|---|---|
| **Health check** | one read-only look at the whole server, as green, yellow and red lines: the emulator (and its port 3000), MariaDB, NGINX, PHP-FPM, Docker, Traefik, Fail2Ban and CrowdSec, the hotel site answering on 127.0.0.1:85, disk (yellow from 80 %, red from 90 %), memory, load against the cores, the https certificate (yellow under 21 days, red under 7), the newest database backup in `/var/backups/all-in-1` (yellow when there is none or it is older than 7 days), pending (security) updates, a reboot the system asks for, the firewall and the clock (NTP). A problem has a **Fix** button that runs the matching tool below (Restart emulator, Create full database backup, Update system, Enable firewall, ...), with its usual question and log. The check runs when the page opens and on **Check again**; nothing on the server is changed. See [Health check and History](#health-check-and-history) |
| **Server status** | opens a page with only the live dashboard (**Back to the tools** at the bottom, or click **Tools** in the sidebar), refreshed every 2 seconds while it is on screen: CPU (with load and cores), memory (and swap), network in/out with a graph that scales between B/s, KB/s and MB/s, uptime, emulator running and players online, disks with how full they are, the users logged in on the server (`who`), and the git branch, commit, date and last change of the emulator, Octane, Octane-Renderer (and AtomCMS), with the emulator's JAR version and the Octane / Renderer package versions. The **Versions** panel also has an **Updates** column: when the page opens (and on **Check for updates**) every clone is fetched and the column says `up to date` or `3 new`, with the newest commit in the tooltip. Everything else is read from `/proc` in one short command per sample; nothing is installed on the server |
| **Emulator & wired** | a live page about the emulator itself, without changing it: its CPU (% of one core and of the server), JVM memory against `-Xmx`, threads and uptime; the CPU of every wired thread (the coordinator that ticks every 50 ms, shards 0–5 that run the rooms' wired, and the heavy-rooms worker); where the emulator's CPU goes per thread group (wired, game logic, game connections, database, garbage collector, JIT, logging, RCON); and the rooms in the log's `Slow wired task: shard=, room=, took=ms` and "wired tasks waiting" warnings since the last start, with room names, slowest and total time, and the latest warnings. CPU comes from `/proc/<pid>/task/*/stat`; the full thread names from `jcmd Thread.print`, read only when new threads appear (a thread dump briefly pauses the JVM) |
| **SSH terminal** | a command line on the server in the window, as the Tools login (root with your key): an xterm-compatible terminal (256 colours and truecolor, UTF-8, the alternate screen), so full-screen programs like `nano`, `vim` and `top` work. The terminal follows the window's size, keeps 5,000 lines of scrollback (wheel or Shift+PgUp), and selecting with the mouse + **Ctrl+Shift+C** copies; **Ctrl+Shift+V**, Shift+Insert or the middle button pastes (bracketed, so a pasted script does not run line by line); right-click has Copy / Paste / Select all / Clear scrollback; Ctrl + / − or **A− / A+** change the text size. The session keeps running while you look at other pages; `exit` ends it, **New session** starts another. Under it is the `ssh` command for an SSH client of your own (OpenSSH needs the key in OpenSSH format, PuTTY the `.ppk`) |
| **Restart emulator** | `systemctl restart emulator` and waits until Polaris has loaded (asks first: everyone is disconnected) |
| **Restart MariaDB** | restarts MariaDB and waits until it answers |
| **Restart NGINX** | `nginx -t` first (a broken configuration is not restarted), then PHP-FPM 8.5 and NGINX; checks 127.0.0.1:85 |
| **Restart Traefik** | `docker compose up -d --force-recreate` in `/opt/traefik`: a new container, with the newest downloaded image (a `docker restart` would keep the old one); the routes and certificates stay. Waits for port 80 |
| **Restart all** | MariaDB, emulator, NGINX + PHP-FPM, Traefik, in that order |
| **Fix visitor IPs** | for a hotel behind Cloudflare whose players show up with a Cloudflare address: gives Polaris (`ws.ip.header.trusted`), NGINX (`/etc/nginx/hotel-real-ip.conf`) and Traefik Cloudflare's current ranges and sets up the weekly refresh (`/var/scripts/CF_Refresh.sh`, `/etc/cron.d/cf-refresh`). Asks first: the emulator restarts when its setting changes, Traefik when its list changes. A hotel set up without Cloudflare gets the advice to run Web access again with Cloudflare on |
| **Enable firewall** | switches ufw on with the Harden step's rules: when that step set them up they are switched on again, after checking that every SSH port (and the one you are connected on) is in them, so you cannot lock yourself out; on a server where it never ran, the same rules are set up (SSH rate limited, 80/443 for everyone or only Cloudflare's ranges, everything else closed) |
| **Disable firewall** | `ufw disable` (asks first: every port is reachable again, also the emulator's 3000 and 2096). The rules are kept, so Enable firewall switches the same rules on again; for testing a connection problem |
| **CrowdSec whitelist and bans** | for the [CrowdSec](#crowdsec) addon: edit the whitelist (CrowdSec's allowlist and the bouncer's trusted addresses; Traefik reads it without a restart, and bans of whitelisted addresses are lifted) and tick banned addresses to let them in again (the bouncer knows within a minute). Only this server's own bans are listed, not the community blocklist |
| **Whitelist and bans** | for the [Fail2Ban](#fail2ban) addon: edit the whitelist (addresses and networks that are never banned, with **Add my IP**) and tick banned addresses to let them in again. Saving rewrites `/etc/fail2ban/jail.d/all-in-1.local` (checked with `fail2ban-client -t` first) and restarts fail2ban; on a server without fail2ban it points to the addon |
| **Create full database backup** | `mariadb-dump --single-transaction --routines --triggers --events` of the hotel database into `/var/backups/all-in-1/<db>-<time>.sql.gz` (root only; the newest 10 are kept), then downloads it over SFTP to a file you choose (owner-only on Linux/macOS) |
| **Import full database** | uploads a `.sql` or `.sql.gz`, makes a safety backup (`<db>-before-import-<time>.sql.gz`), stops the emulator, recreates the database with its character set and imports (database names in `CREATE DATABASE` / `USE` lines become the hotel's), clears AtomCMS' cache, and starts the emulator, which updates the database to its version. A file that is not a SQL dump is refused before anything changes |
| **Furniture language** | the furniture names and descriptions in another hotel's language (the 9 Habbo hotels): its furnidata is downloaded and converted, the hotel's own furni stay, and `items_base` / `catalog_items` follow it (sprite ids, catalogue items, offer ids), after a safety backup and with the emulator restarted. See [Default catalogue](#default-catalogue) |
| **Import Plus database** | a PlusEMU dump (such as Plus' fresh install) into the Polaris hotel database: the reverse of [plus-migrator](https://github.com/plus-emulator/plus-migrator). Choose **Furniture and catalog** or **Furniture only**. It uploads the `.sql` / `.sql.gz`, makes a safety backup (`<db>-before-plus-import-<time>.sql.gz`), stops the emulator, loads the dump into a staging database `plus_import` next to the hotel's, converts, drops the staging database and starts the emulator. See [Import Plus database](#import-plus-database) |
| **Check multiheight** | compares every floor and wall furni in `items_base` that has a `multiheight` with the states of its `.hab` in `gamedata/furniture` (counted as the converter counts interaction modes: the first visualization with animations, its highest animation id + 1). It lists furni with **too many heights** (the last ones show no graphic, e.g. `0.8;2.0;3.0;4.0;5.0` for 4 states), a wrong `interaction_modes_count`, **too few heights** (states that cannot be reached; add the heights by hand, they are not in the `.hab`) and furni without a readable `.hab`. After a yes, too many heights are cut to one per state (the first heights stay) and `interaction_modes_count` is set to match, in one transaction that only changes rows still as they were read; then the emulator restarts. Needs Node.js on the server (installed with Octane) |
| **Recover SQL password** | gives MariaDB's root a new random 32-character password (asks first) and saves it in `configuration/database/<server>/mariadb.txt`; a changed file is kept as `mariadb.txt.bak-<time>`. Changes `root@localhost` (its socket login stays) and `root@'%'` when it exists. When root cannot log in through the socket any more, MariaDB is restarted for a moment with `--skip-grant-tables --skip-networking` to change it and then started normally. Then it tests the new password over 127.0.0.1 and the socket. Without a saved file, the CMS and emulator accounts are read from AtomCMS' `.env` and the emulator's `config.ini`, so the new file is complete. The emulator and AtomCMS use their own accounts and are not affected |
| **Check for updates** | `git fetch` of the emulator, Octane, Octane-Renderer and AtomCMS, and per clone how many commits a `git pull` would bring in, with the newest one. The commits are counted against the branch chosen in **Settings → Repository branches**, not against the branch the clone happens to be on, so a clone still on `main` while `dev` is chosen reports what `dev` has and says `main → dev`. AtomCMS has no choice and is counted against its own branch. A fetch only fills the remote branches: no file on the server is changed and the hotel keeps running. The same check runs by itself right after you log in here, and again after an update, so **Update emulator** and **Update Octane** say `3 new commit(s)` or `up to date` before you press them |
| **Update system** | `apt-get update`, `upgrade` and `dist-upgrade` (changed configuration files are kept); says when a reboot is needed |
| **Update emulator** | `git pull` in `/var/www/emulator` on the branch chosen in **Settings → Repository branches** (a clone on another branch is switched over first) and the missing files of its `Assets/` copied into gamedata (see Deploy Assets → Images); when there are new commits: `build.sh` and a restart. The start script runs the newest `target/*-jar-with-dependencies.jar`, so a new version number is picked up by itself; the result shows old → new version. No new commits: nothing is rebuilt or restarted |
| **Update Octane** | `git pull` of Octane-Renderer and Octane, both on the branch chosen in **Settings → Repository branches** (a clone on another branch is switched over first), then in both `rm -rf node_modules yarn.lock` and `yarn install`, then the configuration for the site (keeps AtomCMS' `/client` layout), `yarn build` and a test. The next pull puts the repository's `yarn.lock` back first |
| **Update AtomCMS** | `git pull` in `/var/www/atomcms`, `composer install`, `php artisan migrate` (an installed AtomCMS only runs the new migrations; its settings and the hotel name stay) and the theme build. Octane is not touched |
| **Update Traefik** | `docker compose pull` and `up -d` in `/opt/traefik`: the newest `traefik:v3` image, and a restart only when it changed (routes, settings and certificates stay; the players are disconnected for a few seconds). When Docker Hub does not answer nothing is changed, and it says so (also for Gitea and CrowdSec) |
| **Update Gitea** | the same for Gitea in `/opt/gitea` (`gitea/gitea:1`); the repositories and accounts stay |
| **Update CrowdSec** | the same for CrowdSec in `/opt/crowdsec` (`crowdsecurity/crowdsec:latest`), then the newest detection rules (`cscli hub update` and `upgrade`; CrowdSec restarts only when a rule changed). The bans, whitelist and enrollment stay, Traefik is not restarted and the hotel stays reachable. It also says when Traefik's CrowdSec plugin is older than the one this installer knows (run Addons → CrowdSec again to update it) |

### Import Plus database

**Tools → Import Plus database** converts PlusEMU content to Polaris, so a Plus database (for example the
`Database/FreshInstall.sql` of PlusEMU) works for the hotel. It is the reverse of
[plus-migrator](https://github.com/plus-emulator/plus-migrator), which goes from Arcturus / Polaris to Plus.

- **Furniture** (`furniture` → `items_base`): furniture Polaris already has (same classname and type) keeps its id and
  settings, so rooms and inventories are not touched. New furniture gets the next free ids and the Polaris
  interaction: Wired boxes by their classname, Plus' renamed types back to the Polaris name (`purchasable_clothing` →
  `clothing`, `crackable_egg` → `crackable`, `pressure_pad` → `pressureplate`, `bb_red_gate` → `battlebanzai_gate_red`
  and so on), and every result is checked against the interactions Polaris registers. A type Polaris does not have
  becomes plain furniture and is listed in the log. Clothing set ids go to `catalog_clothing`; multi-heights and
  vending lists get Polaris' separators.
- **Catalog** (only with **Furniture and catalog**): the normal catalog is replaced by the Plus one. An offer on several
  pages becomes one catalog item per page; bundles become `id;id:amount`; effects, badges, bots and pets become the
  items Polaris sells them as (`avatar_effect<n>`, the badge code, `bot_generic` / `bot_bartender` with the bot's look,
  `a0 pet<n>`). Builders Club pages and the Habbicons shop stay as Polaris has them. Page texts and images go to the
  columns each Polaris layout reads.
- **Safe to run again**: a second run adds nothing new and builds the same catalog, and repairs clothing set ids an
  earlier import wrote with spaces (`3373, 3374`, which Polaris cannot read). When the conversion stops halfway, or
  the emulator does not start with the converted database, the safety backup is put back right away and the emulator
  starts on it.
- **After the import**: the client needs the new furniture in its furnidata and assets (**Deploy Assets**). New
  crackables have no prizes yet (`items_crackable`): until you add them, clicking does nothing. The log lists the
  numbers: furniture added and already there, per interaction rule, and what was left out.

The conversion is `source/crates/remote/sql/plus-to-polaris-furniture.sql` and `plus-to-polaris-catalog.sql`, built into
the program.

**Settings backup** (on the **Settings** page, no server needed): **Export encrypted settings** writes the whole configuration directory
(SSH keys, .ppk files, MariaDB passwords of every server) to one `.aio-settings` file, encrypted with AES-256-GCM
and a key derived from your password (PBKDF2-SHA256, 600,000 rounds). **Import encrypted settings** on another
computer restores it; the settings already there are kept next to it as `configuration.bak-<time>`. A wrong
password or a damaged file changes nothing.

![Server status](public/docs/screenshots/server-status.png)

![Emulator & wired](public/docs/screenshots/emulator-wired.png)

![SSH terminal](public/docs/screenshots/tools-terminal.png)

### Health check and History

![Health check](public/docs/screenshots/health-check.png)

**History** (in the sidebar under TOOLS) keeps every install step and tool you run, with its result and its whole
log, per server: `configuration/history/<server>/<yyyyMMdd-HHmmss>-<title>.log`, a plain text file with a short
header (title, step or tool, outcome, start and end) and the log. The page lists the runs newest first with **DONE**,
**WARNING**, **FAILED** or **ABORTED**; choose **This server**, **All servers** or one server, filter on title,
result or server, and click a run to read its log. The newest 300 runs per server are kept. Dry runs are not
recorded.

![History](public/docs/screenshots/history.png)

## Updating the installer itself

**Renamed from All-in-1 Installer.** The repository was `duckietm/All-in-1-installer` and the program
`all-in-1-installer`. Releases still carry `all-in-1-installer-windows-x64.exe` / `-linux-x64` next to the new names,
so a version from before the rename updates itself to the All-in-1 Manager. Its settings stay: the `configuration`
folder next to the program, or `All-in-1 Installer\configuration` in the user's data folder while that one exists;
files the installer wrote on a server are recognised by both names, and the old history files are still read.

The program checks at startup whether a newer release of <https://github.com/duckietm/All-in-1-Manager> is out
(releases tagged `rust-v<version>`). When there is one, it says what is new and offers **Update and restart**: the
release file is downloaded next to the running program, the running one is renamed to `<name>.old`, the new one takes
its place under the same name, and the program starts again. **Later** keeps the version you have; the question comes
back at the next start. The `.old` file is removed at that next start. A download that stops early never replaces a
working program.

The check is quiet when there is nothing to do: no internet, a release that is not newer, a draft or pre-release, or
a repository without releases all mean no question is asked. A debug build (`cargo run`) never offers an update.
**About → Check for updates** checks again at any time.

The version it compares is `version` in `[workspace.package]` of `Cargo.toml` against the release's tag. The file it
downloads is `all-in-1-manager-windows-x64.exe` on Windows and `all-in-1-manager-linux-x64` on Linux.

## Building

Requires Rust (stable, 1.85 or newer) from <https://rustup.rs>. On Linux also the libraries of the window:

```bash
sudo apt-get install -y pkg-config libxkbcommon-dev libgl1-mesa-dev libgtk-3-dev
```

The quickest way: run the build script in the root of the repository. On Windows double-click **`build.cmd`** for a
menu, or run it in a terminal; on Linux use `./build.sh`:

```
build release      ./build.sh release     # the program to hand out -> public/release/ (with Assets/ next to it)
build dev          ./build.sh dev         # debug build             -> public/dev/
build run          ./build.sh run         # debug build, and start it
build test         ./build.sh test        # all tests
```

On Windows it needs Visual Studio's "Desktop development with C++" (Rust uses its linker). The same with Python:

```bash
python source/tools/build.py dev               # debug build    -> public/dev/all-in-1-manager(.exe)
python source/tools/build.py release           # release build  -> public/release/ (with Assets/ next to it)
python source/tools/build.py dev --run         # and start it
cargo test --workspace                         # all tests, including the parity tests
```

`public/dev` and `public/release` are where you run the program while developing and the copy you hand out; each
keeps its own `configuration/` (keys, passwords, history) next to the program, and git ignores both. Plain
`cargo build` / `cargo run -p aio-app` work too and leave the program in `target/`.

On Windows both the MSVC and the GNU toolchain work (the GNU one with MinGW, for the icon and version information of
the `.exe`). The installer runs on your own Windows or Linux desktop and connects to the server, so the server itself
needs no GUI.

### Dry run

```bash
all-in-1-manager --dry-run
```

logs in to nothing: every command a step would send to the server (the scripts, what goes over stdin, uploads and
downloads) is shown in that step's log, prefixed with `[dry run]`, instead of being run. Every command "succeeds" with
no output, so the steps take the path of a fresh server. Use it to read exactly what the installer would do.

### Screenshots

`AIO_SCREENSHOT=<file.png>` saves one screenshot of the window and closes it; `AIO_SCREENSHOT_PAGE` picks the page
(`dashboard`, `install`, `cms`, `customcms`, `epicnext`, `gitea`, `fail2ban`, `crowdsec`, `deploy`, `explorer`, `sql`,
`debug`, `tools`, `status`, `doctor`, `history`, `monitor`, `terminal`, `settings`, `about`, `login`),
`AIO_SCREENSHOT_STEP=<n>` the n-th step of that page's wizard, `AIO_SCREENSHOT_LANGUAGES=1` opens the language list, `AIO_SCREENSHOT_CONNECT=1` logs in to the dry run
first (the health check then finds a sample server), and `AIO_SCREENSHOT_THEME=dark|light`,
`AIO_SCREENSHOT_LOOK=hotel|professional` and `AIO_SCREENSHOT_LANG=<code>` set the rest. For example:

```bash
AIO_SCREENSHOT=install.png AIO_SCREENSHOT_PAGE=install AIO_SCREENSHOT_STEP=2 AIO_SCREENSHOT_CONNECT=1 all-in-1-manager
```

The pictures in `public/docs/screenshots` are made this way with the latest release, in English and the light theme
(the dark ones in the dark theme), from a folder whose `configuration/settings.json` lists a few sample servers.

### Parity with the C# installer

Every script, SQL statement and file the installer sends to the server comes from the C# installer's code, and the
tests prove it is the same text, byte for byte: `source/crates/remote/tests/golden/*.txt` (about 350 files) were written by
the C# code itself (the last C# version, 1.0.2), through a harness that compiled its `Remote/*.cs` against a recording
connection, for every install step and tool and their variants (https/http, Cloudflare, a CMS, a re-run ...). The
Rust steps run the same scenarios against the recording server of `aio-remote` and must produce the same files.
The same was done for the terminal emulator (27 screen dumps). The harnesses (`tools/parity`, `tools/parity-terminal`) and
the C# sources they compile are in the git history up to commit `50dd986`.

### On GitHub

`.github/workflows/ci.yml` tests and builds both programs on every push to `main` and on every pull request, on
`windows-latest` and `ubuntu-latest`, and keeps them as workflow artifacts.

Every push to `main` makes a release (`.github/workflows/release.yml`), and the version goes up by itself: when the
version in `Cargo.toml` already has a release, the last number is raised (2.0.3 → 2.0.4), the bot commits
`Version 2.0.4` to `main`, and that commit is tested, built and published as `rust-v2.0.4` with
`all-in-1-manager-windows-x64.exe` and `all-in-1-manager-linux-x64`, the files the update check looks for. A
version you set yourself and that has no release yet (for a bigger step, e.g. 2.1.0) is released as it is. Pushes
that only change the README, the licence or `public/docs` release nothing. After a release, `git pull` brings the
bot's version commit to your copy.

`source/tools/version.py` shows the version and changes it in `Cargo.toml`, `Cargo.lock` and the Windows version
information (`app.rc`) together:

```bash
python source/tools/version.py              # print it
python source/tools/version.py --set 2.1.0  # set your own
```

## Project layout

```
Cargo.toml                       The workspace (version of the program in [workspace.package])
README.md, LICENSE               About the program (this file)
public/                          What you read and run
  docs/screenshots/              The pictures in this README
  dev/, release/                 The program after source/tools/build.py, each with its configuration/ (not in git)
source/                          The code
  tools/                         build.py, version.py, i18n_keys.py (texts to translate), make_icon.py (the program icon)
  Assets/                        Client images packed into the program (Deploy Assets → Images)
  crates/core/                   What lives on this computer
    src/settings.rs              configuration/settings.json: servers, default key, branches, look, layout, language, assets, retros
    src/layout.rs                Paths on the server, the emulator's account and the database name, with their checks
    src/config_dir.rs            The configuration/ folder: keys per server, mariadb.txt, owner-only files
    src/credentials.rs           Generated MariaDB passwords and their file format
    src/keys.rs                  SSH keys: generation (encrypted OpenSSH + PuTTY .ppk v2), public key formats, .ppk reader
    src/backup.rs                Encrypted export / import of the configuration folder (AES-256-GCM, PBKDF2)
  crates/assets/                 Habbo's asset bundles: .hab, .nitro and .asar read and written, and the
                                 conversions to .hab every part of the installer uses (no network, no server)
  crates/remote/                 Everything that happens on the server
    src/server.rs                The Server trait: commands as root, streaming, follow, stdin, SFTP, tunnel, shell
    src/ssh.rs                   The SSH login (russh): password, keyboard-interactive or key, the OS check, sudo -> root
    src/recorder.rs              A Server that records instead of running (dry run, parity tests)
    src/script.rs, prelude.sh    The script runner and the prelude every install script starts with
    src/nginx.rs, php.rs, mariadb.rs, dev_tools.rs, git_clone.rs, emulator.rs, web_access.rs, hotel_site.rs,
    src/octane.rs, harden.rs, ssh_login.rs, root_authorized_keys.rs    The install steps
    src/atom_cms.rs, custom_cms.rs, epicnext.rs, cms_site.rs, local_ports.rs, gitea.rs, fail2ban.rs, crowdsec.rs
                                 The addons
    src/asset_sync.rs, ui_texts.rs, emulator_assets.rs, gamedata_files.rs   Deploy Assets and the Asset Explorer
    src/sql_session.rs, sql_script.rs  SQL Editor: MariaDB over the SSH tunnel, statements like the mariadb client
    src/server_tools.rs, plus_import.rs, server_status.rs, emulator_monitor.rs, server_users.rs   The Tools page
    sql/plus-to-polaris-*.sql    The Plus → Polaris conversion
    tests/golden/                What the C# installer sends, per scenario; tests/golden_*.rs compare with it
  crates/terminal/               The xterm-compatible terminal emulator of the SSH terminal
  crates/app/                    The window (egui)
    src/gui/                     Theme (Hotel / Professional), icons, widgets, dialogs, log view, step framework, wizard
    src/views/                   One file per page or step (dashboard, login, the steps, addons, tools, settings ...)
    src/update.rs                The update at startup
    assets/i18n/*.json           The translations (English is the key); source/tools/i18n_keys.py lists what is missing
```

## Credits

**About** in the sidebar shows the version and the same list.

| | |
|---|---|
| **DuckieTM** | The installer, and the [All-in-1 Converter](https://github.com/duckietm/all-in-1-converter) this look and feel comes from. |
| **Life** | The branch choice (main / dev) for the clone steps and in Settings, the update check for the hotel's repositories, and the installer's own update at startup. The Rust port: one program for Windows and Linux, the window in nine languages, and the dry-run mode. |
