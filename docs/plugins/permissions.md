# Permissions

A plugin lists what it may do in the `permissions` array of its `manifest.json`. Before a plugin is installed (or a
developer folder loaded), the Manager's consent window shows each permission as a plain sentence, and nothing happens
until the user agrees. Every host call is checked against this list: anything not covered fails with
[`ErrorKind::Denied`](api.md#errors) and a `permission denied: ...` line in the plugin log.

Ask for the least you need. Users read this list, and the official index's reviewers ask you to justify every entry.

## The permission strings

| Permission | Allows | What the user reads |
|---|---|---|
| `files` | The private folder ([`files`](api.md#files)). Always allowed: the Manager does not enforce it, but declare it so users see that the plugin keeps files. | Keep its own files in a private folder (always allowed) |
| `ssh` | [`script::run`](api.md#script): scripts on the hotel's server as the logged-in user, each one approved by the user. | Run commands on the hotel's server (you approve each script) |
| `ssh:root` | [`script::run_as_root`](api.md#script): scripts as root, each one approved. Shown with a large red warning. | Run commands as root on the hotel's server (you approve each script) |
| `sql:read:<tables>` | [`Select`](api.md#reading) on these tables. | Read the table `<tables>` of the hotel database (one table) / Read the tables `<t1>, <t2>` of the hotel database (several) |
| `sql:write:<tables>` | [`sql::write`](api.md#changing): insert, update and delete in these tables, each batch approved by the user. | Change the table `<tables>` of the hotel database (you approve each change) / Change the tables `<t1>, <t2>` of the hotel database (you approve each change) |
| `sql:read-private:<table>.<column>` | Read, filter on, or write one [private column](#private-columns). Needs `sql:read` or `sql:write` for the table as well. | Read the private column `<column>` of the table `<table>` (it can hold passwords, addresses or tokens) |
| `http:<host>` | [`http::get`](api.md#http) and `http::get_many` to exactly this host, on the default port. | Fetch pages from `<host>` on the internet |
| `http:ask` | **API 1.1.** [`http::get`](api.md#http) and `http::get_many` to a public host that no `http:<host>` names, after the user allows that host. See [Asking for hosts](#asking-for-hosts-httpask). Shown in the warning colour. | Ask you before fetching from any other internet host |
| `tcp:localhost:<port>` | [`tcp::connect`](api.md#tcp) to `127.0.0.1:<port>`. | Connect to a program on this computer, port `<port>` (for example G-Earth) |
| `tcp:localhost:<a>-<b>` | **API 1.1.** [`tcp::connect`](api.md#tcp) to any port from `<a>` to `<b>`, at most 16 ports. | Connect to programs on this computer, ports `<a>`–`<b>` (for example G-Earth) |

`aio-plugin check` prints the same sentences, so you can see what users will read:

```text
permissions:
  - files: Keep its own files in a private folder (always allowed)
```

The host calls of [`host`](api.md#host) (log, status line, questions, text prompts, file dialogs, `keep_working`,
`open_manager_page`), the folder picker [`folders::pick`](api.md#folders) with everything a picked `Folder` does
(writing new files and reading or listing what is in it) and the [page](ui.md) need no permission: the user picks the
folder each time, so picking it is the consent.

The permissions marked **API 1.1** need `"api": "1.1"` in the manifest; with `"1.0"` the manifest is refused with
`The permission <p> needs API 1.1 ("api": "1.1" in the manifest)`.

## Grammar

| Part | Rule |
|---|---|
| `<tables>` | One table, or several separated by commas without spaces: `sql:read:users,rooms`. Each name has 1 to 64 characters `A-Z a-z 0-9 _`. |
| Table names | Matched exactly, case included: `sql:read:users` does not allow a query on `Users`. Write the name in the query as in the manifest. |
| `<table>.<column>` | Two names of 1 to 64 characters `A-Z a-z 0-9 _`, with one dot between them. |
| `<host>` | 1 to 253 characters `A-Z a-z 0-9 . -`, not starting with `.` or `-`. No scheme, no port, no path, no `*`. Matched ignoring case; subdomains are separate hosts. |
| `<port>` | A number from 1 to 65535. |
| `<a>-<b>` | Two ports with a `-` between them and no spaces, the smaller first: `tcp:localhost:9092-9101`. At most 16 ports (`<b>` − `<a>` ≤ 15). |

A manifest lists at most 64 permissions. Repeats are refused: the same permission twice, the same table twice for the
same access (also across two `sql:read` entries), or the same host twice in any case. An unknown or malformed string
refuses the whole manifest; see [Troubleshooting](troubleshooting.md#a-refused-manifest-or-package) for the messages.

Example:

```json
"permissions": [
  "files",
  "sql:read:users,rooms",
  "sql:write:rooms",
  "http:api.example.com",
  "tcp:localhost:9092"
]
```

With API 1.1, also:

```json
"api": "1.1",
"permissions": ["files", "http:ask", "tcp:localhost:9092-9101"]
```

## Private columns

Some database columns hold secrets. The Manager treats these columns as private (names compared ignoring case):

- on the table `users`: `password`, `mail`, `ip_register`, `ip_current`, `auth_ticket`, `machine_id`;
- on every table: a column named exactly `ip` or `machine_id`;
- on every table: a column whose name contains `password`, `token`, `secret` or `ip_`.

A private column needs its own `sql:read-private:<table>.<column>` permission, on top of `sql:read:<table>` or
`sql:write:<table>`, to be:

- read (named in `Select::columns`);
- used in a condition (`filter` of a select, an update or a delete), because a comparison could probe a password hash;
- written (set by an insert or an update), because setting a password or a login ticket takes over an account.

Without it, the call fails with `permission denied: column 'users.mail' is private (needs
sql:read-private:users.mail)`. Users see each private column as a separate sentence that warns about passwords,
addresses and tokens, and reviewers look at these closely. Most plugins never need one.

## Approvals

Some permissions only allow a plugin to *ask*. Nothing runs on the server and nothing changes in the database until
the user approves it:

- `ssh` / `ssh:root`: each `script::run` or `script::run_as_root`;
- `sql:write`: each `sql::write` batch.

The plugin's page then shows an **approval card** at the top, above the plugin's content, with the whole script (or
every statement with every value), the server, and for a script the user it runs as; a script that runs as root is
marked "Runs as root". The user picks:

- **Approve**: the script runs, or the batch is applied in one transaction. The call returns the result.
- **Reject**: the call returns `ErrorKind::Rejected` (`the user rejected the script` or `the user rejected the database
  changes`).
- **Reject and stop this plugin**: the same, and the plugin is stopped.

The call waits as long as the user takes; that time does not count against any limit. An approval belongs to the
server that was logged in when the plugin asked: if the login changed meanwhile, Approve fails with `The server
changed since the request` (`ErrorKind::Other`) and nothing runs. Closing the page or stopping the plugin rejects what
it was waiting for.

What is shown must be what runs, so the Manager refuses a script it cannot show exactly (empty, larger than 64 KiB,
a carriage return or another control character, invisible or text-direction characters) and a batch too large to show
(more than 1,000 operations or 8 MiB of values). Keep scripts short and readable: the user decides on what they read.

## Asking for hosts (`http:ask`)

**API 1.1.** A plugin that cannot know its hosts in advance (a user types a URL, a catalogue names its own image
server) declares `http:ask`. Its hosts named with `http:<host>` work as before; a request to any other host goes like
this:

1. Only a public host name can be asked about. An IP address (also in hex or other number forms), `localhost` and
   names ending in `.localhost`, single-label names (`nas`), and names ending in `.local`, `.lan`, `.internal` or
   `.home.arpa` are refused without asking (`permission denied: host <host> cannot be asked for`).
2. The first time, the Manager asks the user: "<plugin> wants to fetch from <host>. Allow?". The dialog counts
   towards the 20 dialogs of a call, and the user's time does not count against the call's 5 s. `http::get_many`
   asks about its hosts one at a time, before it fetches anything.
3. **Yes** is remembered for the plugin: later requests to that host need no question. At most 64 hosts are
   remembered per plugin; past that the call fails with `permission denied: at most 64 hosts can be allowed; forget
   one on the Plugins page`.
4. **No** is remembered until the Manager closes: further requests to that host fail at once with
   `permission denied: host <host> was refused`, without asking. After 256 refused hosts in one session, every other
   host is refused without asking.
5. When the request runs, a host whose addresses are all on this computer or the local network (loopback, private,
   link-local, ...) is refused: `permission denied: host <host> resolves to a local address`. The check is made on
   the addresses actually connected to, for the request and for every redirect.

The user sees the remembered hosts on the plugin's card on the Plugins page, under **Hosts you allowed**, each with
**Forget** (the plugin then asks again). An update of the plugin keeps them. A reinstall of the same version, a
package by another author, a version without `http:ask`, a revocation and a removal forget them. A developer folder
remembers them only until the Manager closes.

Prefer `http:<host>` for every host you know: users and reviewers read a fixed list more easily than a promise to ask.

## When permissions change

An update that asks for more permissions shows the consent window again, with the new ones marked "new in this
version"; the old version keeps running until the user clicks **Install**. A developer folder whose permissions grew
on **Reload** stays off until the user reviews them again.
