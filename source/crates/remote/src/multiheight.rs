//! Check multiheight: every floor and wall furni in items_base that has a multiheight, compared with the states of its
//! .hab in /var/www/gamedata/furniture. Arcturus-based emulators cycle a multiheight furni through its heights, so a
//! height without a state shows no graphic, and a state without a height cannot be reached. The states are counted
//! as the All-in-1 converter counts interaction modes (Tools/Generate_SQL.cs InteractionModesCountFromNitroJson): the
//! first visualization with animations, its highest animation id + 1 when it has animation 0, else how many it has.

use crate::script::{self, fill};
use crate::server::{Log, Result, Server, ServerError};
use crate::server_tools;

/// One furni with a multiheight, and what its .hab says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: i64,
    pub item_name: String,
    pub multiheight: String,
    pub interaction_modes_count: i64,
    /// The states of its .hab; None when the .hab is missing or unreadable.
    pub states: Option<usize>,
}

impl Row {
    /// The heights of the multiheight, trimmed, without empty parts.
    pub fn heights(&self) -> Vec<&str> {
        self.multiheight.split(';').map(str::trim).filter(|h| !h.is_empty()).collect()
    }

    pub fn verdict(&self) -> Verdict {
        let heights = self.heights().len();
        match self.states {
            None => Verdict::NoHab,
            Some(states) if heights == states => {
                if self.interaction_modes_count == states as i64 {
                    Verdict::Ok
                } else {
                    Verdict::WrongCount
                }
            }
            Some(states) if heights > states => Verdict::TooMany,
            Some(_) => Verdict::TooFew,
        }
    }

    /// The multiheight with only as many heights as the .hab has states (for TooMany).
    pub fn fixed_multiheight(&self) -> Option<String> {
        let states = self.states?;
        let heights = self.heights();
        (heights.len() >= states && states > 0).then(|| heights[..states].join(";"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    /// As many heights as states, but interaction_modes_count differs.
    WrongCount,
    /// More heights than states: the last heights show no graphic. Fixable: keep the first ones.
    TooMany,
    /// Fewer heights than states: some states cannot be reached. Not fixable here (the heights are not in the .hab).
    TooFew,
    NoHab,
}

// The .hab reader: "HAB\0", u16 version, u16 flags, u32 index length, u32 raw index length, u32 data length, the zlib
// JSON index and the data; the first .json entry is the furni's JSON. Prints "id\tstates" (-1: no readable .hab).
const COUNT_SCRIPT: &str = r#"const fs = require('fs'), path = require('path'), zlib = require('zlib');
const dir = process.argv[2];
const inflate = (b) => { try { return zlib.inflateSync(b); } catch (e) { return zlib.inflateRawSync(b); } };
const states = (json) => {
  for (const viz of json.visualizations || []) {
    if (!viz || typeof viz.animations !== 'object' || viz.animations === null) continue;
    const ids = Object.keys(viz.animations).filter((k) => /^\d+$/.test(k)).map(Number).filter((n) => n >= 0 && n < 50);
    if (ids.length) return ids.includes(0) ? Math.max(...ids) + 1 : ids.length;
  }
  return 1;
};
const cache = new Map();
const count = (name) => {
  if (cache.has(name)) return cache.get(name);
  let result = -1;
  try {
    const d = fs.readFileSync(path.join(dir, name + '.hab'));
    if (d.length >= 20 && d.toString('latin1', 0, 4) === 'HAB\0') {
      const il = d.readUInt32LE(8), base = 20 + il;
      const index = JSON.parse(inflate(d.subarray(20, base)).toString('utf8'));
      const entry = (index.entries || []).find((e) => /\.json$/i.test(e.name));
      if (entry) {
        const raw = d.subarray(base + entry.offset, base + entry.offset + entry.storedLength);
        const text = (entry.compression === 'deflate' ? inflate(raw) : raw).toString('utf8').replace(/^﻿/, '');
        result = states(JSON.parse(text));
      }
    }
  } catch (e) { result = -1; }
  cache.set(name, result);
  return result;
};
let input = '';
process.stdin.on('data', (c) => (input += c));
process.stdin.on('end', () => {
  for (const line of input.split('\n')) {
    const [id, name] = line.split('\t');
    if (!id || !name) continue;
    console.log(`${id}\t${count(name.split('*')[0].trim())}`);
  }
});"#;

const CHECK: &str = r#"{{DatabaseName}}
command -v node >/dev/null 2>&1 || fail "Node.js is not installed on this server (the Octane step installs it)."
[ -d {{Furniture}} ] || fail "{{Furniture}} does not exist: deploy the furniture first."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Floor (s) and wall (i) furni with a multiheight; pets (a0 ...) have no furni bundle. HEX keeps every character.
mariadb -N -B "$db" -e "SELECT id, item_name, HEX(multiheight), interaction_modes_count FROM items_base
  WHERE multiheight IS NOT NULL AND multiheight NOT IN ('', '0') AND type IN ('s', 'i') AND item_name NOT LIKE 'a0%' ORDER BY id" > "$tmp/rows" ||
  fail "Reading items_base failed (see above)."
cat > "$tmp/count.js" <<'JS'
{{CountScript}}
JS
cut -f1,2 "$tmp/rows" | node "$tmp/count.js" {{Furniture}} > "$tmp/states" || fail "Reading the .hab files failed (see above)."
echo "@rows"
cat "$tmp/rows"
echo "@states"
cat "$tmp/states""#;

fn unhex(hex: &str) -> String {
    let bytes: Vec<u8> = (0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn parse(output: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut states = std::collections::HashMap::new();
    let mut section = "";
    for line in output.lines() {
        match line {
            "@rows" | "@states" => section = line,
            _ => {
                let f: Vec<&str> = line.split('\t').collect();
                if section == "@rows" && f.len() >= 4 {
                    if let Ok(id) = f[0].parse() {
                        rows.push(Row {
                            id,
                            item_name: f[1].to_string(),
                            multiheight: unhex(f[2]),
                            interaction_modes_count: f[3].trim().parse().unwrap_or(0),
                            states: None,
                        });
                    }
                } else if section == "@states" && f.len() >= 2 {
                    if let (Ok(id), Ok(n)) = (f[0].parse::<i64>(), f[1].trim().parse::<i64>()) {
                        states.insert(id, n);
                    }
                }
            }
        }
    }
    for row in &mut rows {
        row.states = states.get(&row.id).filter(|n| **n > 0).map(|n| *n as usize);
    }
    rows
}

/// Every furni with a multiheight, with the states of its .hab.
pub async fn check(server: &dyn Server) -> Result<Vec<Row>> {
    let furniture = format!("{}/furniture", crate::octane::gamedata());
    let body = fill(CHECK, &[("DatabaseName", &server_tools::database_name()), ("Furniture", &furniture), ("CountScript", COUNT_SCRIPT)]);
    let result = server.run_as_root(&script::with_prelude(&body)).await?;
    if !result.succeeded() {
        let message = format!("{}{}", result.output, result.error);
        let reason = message.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(ServerError(format!("Checking the multiheights failed: {reason}")).into());
    }
    Ok(parse(&result.output))
}

/// The UPDATE statements of the fixable rows (TooMany: the first heights kept; WrongCount: the count set), in one
/// transaction. Each statement checks the old multiheight, so a row edited in the meantime is left alone.
pub fn fix_sql(rows: &[Row]) -> String {
    let mut sql = String::from("SET NAMES utf8mb4;\nSTART TRANSACTION;\n");
    for row in rows {
        let Some(states) = row.states else { continue };
        let multiheight = match row.verdict() {
            Verdict::TooMany => row.fixed_multiheight(),
            Verdict::WrongCount => Some(row.multiheight.clone()),
            _ => None,
        };
        if let Some(multiheight) = multiheight {
            sql += &format!(
                "UPDATE items_base SET multiheight = {}, interaction_modes_count = {states} WHERE id = {} AND multiheight = {};\n",
                script::sql_literal(&multiheight),
                row.id,
                script::sql_literal(&row.multiheight)
            );
        }
    }
    sql += "COMMIT;\n";
    sql
}

const APPLY: &str = r#"{{DatabaseName}}
out=$(mktemp)
trap 'rm -f "$out"' EXIT
echo "> items_base: {{Count}} furni get one height per state"
if ! mariadb "$db" > "$out" 2>&1; then
  { grep -E '^ERROR' "$out" || tail -n 3 "$out"; } | head -n 5 | cut -c1-300 | sed 's/^/  /'
  fail "Fixing the multiheights failed (see above); items_base is as it was."
fi
echo "  done""#;

/// Writes the fixes of `rows` (the fixable ones) in one transaction. Returns how many rows it changes.
pub async fn fix(server: &dyn Server, rows: &[Row], log: Log<'_>) -> Result<usize> {
    let fixable: Vec<&Row> = rows.iter().filter(|r| matches!(r.verdict(), Verdict::TooMany | Verdict::WrongCount)).collect();
    if fixable.is_empty() {
        return Ok(0);
    }
    for row in &fixable {
        log(&format!(
            "  {}: multiheight {} → {}, interaction_modes_count {} → {}",
            row.item_name,
            row.multiheight,
            row.fixed_multiheight().unwrap_or_else(|| row.multiheight.clone()),
            row.interaction_modes_count,
            row.states.unwrap_or(0)
        ));
    }
    let owned: Vec<Row> = fixable.into_iter().cloned().collect();
    let body = fill(APPLY, &[("DatabaseName", &server_tools::database_name()), ("Count", &owned.len().to_string())]);
    script::run(server, &script::with_prelude(&body), log, "Fixing the multiheights failed", Some(&fix_sql(&owned))).await?;
    Ok(owned.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, multiheight: &str, count: i64, states: Option<usize>) -> Row {
        Row { id: 7, item_name: name.into(), multiheight: multiheight.into(), interaction_modes_count: count, states }
    }

    #[test]
    fn verdicts() {
        // The pirate_stage of the bug report: 4 states, 5 heights.
        let stage = row("pirate_stage", "0.8;2.0;3.0;4.0;5.0", 4, Some(4));
        assert_eq!(stage.verdict(), Verdict::TooMany);
        assert_eq!(stage.fixed_multiheight().as_deref(), Some("0.8;2.0;3.0;4.0"));
        assert_eq!(row("a", "0.8;2.0", 2, Some(2)).verdict(), Verdict::Ok);
        assert_eq!(row("a", "0.8; 2.0;", 1, Some(2)).verdict(), Verdict::WrongCount, "spaces and a trailing ; do not count");
        assert_eq!(row("a", "0.8", 1, Some(3)).verdict(), Verdict::TooFew);
        assert_eq!(row("a", "0.8;1", 2, None).verdict(), Verdict::NoHab);
    }

    #[test]
    fn output_is_parsed() {
        let output = "noise\n@rows\n12\tpirate_stage\t302E383B322E303B332E303B342E303B352E30\t4\n13\tchair*2\t302E35\t1\n@states\n12\t4\n13\t-1\n";
        let rows = parse(output);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].multiheight, "0.8;2.0;3.0;4.0;5.0");
        assert_eq!(rows[0].states, Some(4));
        assert_eq!(rows[1].states, None, "-1: no readable .hab");
    }

    #[test]
    fn fixes_check_the_old_value() {
        let sql = fix_sql(&[row("pirate_stage", "0.8;2.0;3.0;4.0;5.0", 4, Some(4)), row("x", "1", 1, Some(3))]);
        assert!(sql.contains("UPDATE items_base SET multiheight = '0.8;2.0;3.0;4.0', interaction_modes_count = 4 WHERE id = 7 AND multiheight = '0.8;2.0;3.0;4.0;5.0';"));
        assert!(!sql.contains("'1'"), "too few heights is not fixed");
        assert!(sql.starts_with("SET NAMES utf8mb4;\nSTART TRANSACTION;\n") && sql.ends_with("COMMIT;\n"));
    }
}
