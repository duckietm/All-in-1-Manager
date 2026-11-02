//! Splits a script into statements like the mariadb client: on ';' outside quotes and comments, with DELIMITER.
//! (SqlScript.cs)
//!
//! The C# code works on UTF-16 chars; this port works on `char`s. Every character the splitter looks at is ASCII,
//! and `char::is_whitespace` / `str::trim` use the same Unicode White_Space set as .NET's `char.IsWhiteSpace` /
//! `string.Trim`, so the statements and line numbers are the same.

/// One statement of a script and the line (from 1) it starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement {
    pub text: String,
    pub line: usize,
}

const DELIMITER: [char; 9] = ['D', 'E', 'L', 'I', 'M', 'I', 'T', 'E', 'R'];

pub fn split(script: &str) -> Vec<SqlStatement> {
    let s: Vec<char> = script.chars().collect();
    let len = s.len();
    let mut statements = Vec::new();
    let mut delimiter: Vec<char> = vec![';'];
    let (mut start, mut line, mut start_line) = (0usize, 1usize, 1usize);
    let mut i = 0usize;
    // Only whitespace and comments so far: such a statement is left out, and DELIMITER is only seen there.
    let mut empty = true;

    let add = |statements: &mut Vec<SqlStatement>, start: usize, end: usize, start_line: usize, empty: &mut bool| {
        let text: String = s[start..end].iter().collect();
        let text = text.trim();
        if !*empty && !text.is_empty() {
            statements.push(SqlStatement { text: text.to_string(), line: start_line });
        }
        *empty = true;
    };

    while i < len {
        let c = s[i];
        if empty && (c.is_whitespace() || c == ';') {
            if c == '\n' {
                line += 1;
            }
            i += 1;
            start = i;
            start_line = line;
            continue;
        }
        if empty && is_delimiter_command(&s, i) {
            let end = index_of(&s, '\n', i).unwrap_or(len);
            let value: String = s[i + DELIMITER.len()..end].iter().collect();
            let value = value.trim();
            if !value.is_empty() {
                delimiter = value.chars().collect();
            }
            i = end;
            start = i;
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            empty = false;
            i = skip_quoted(&s, i, &mut line);
            continue;
        }
        if c == '#' || (c == '-' && i + 1 < len && s[i + 1] == '-' && (i + 2 == len || s[i + 2].is_whitespace())) {
            i = index_of(&s, '\n', i).unwrap_or(len);
            continue;
        }
        if c == '/' && i + 1 < len && s[i + 1] == '*' {
            // /*! ... */ is executed by MariaDB, so it counts as content.
            if i + 2 < len && s[i + 2] == '!' {
                empty = false;
            }
            let end = (i + 2..len.saturating_sub(1)).find(|&j| s[j] == '*' && s[j + 1] == '/').map(|j| j + 2).unwrap_or(len);
            line += s[i..end].iter().filter(|&&ch| ch == '\n').count();
            i = end;
            continue;
        }
        if s[i..].starts_with(&delimiter) {
            add(&mut statements, start, i, start_line, &mut empty);
            i += delimiter.len();
            start = i;
            start_line = line;
            continue;
        }
        if c == '\n' {
            line += 1;
        }
        if !c.is_whitespace() {
            empty = false;
        }
        i += 1;
    }
    add(&mut statements, start, len, start_line, &mut empty);
    statements
}

fn index_of(s: &[char], c: char, from: usize) -> Option<usize> {
    s[from..].iter().position(|&x| x == c).map(|p| p + from)
}

fn is_delimiter_command(s: &[char], i: usize) -> bool {
    let n = DELIMITER.len();
    i + n < s.len() && s[i..i + n].iter().zip(DELIMITER).all(|(a, b)| a.eq_ignore_ascii_case(&b)) && matches!(s[i + n], ' ' | '\t')
}

// Returns the index after the closing quote. Backslash escapes, and a doubled quote is a quote.
fn skip_quoted(s: &[char], mut i: usize, line: &mut usize) -> usize {
    let quote = s[i];
    i += 1;
    while i < s.len() {
        let c = s[i];
        if c == '\n' {
            *line += 1;
        }
        if c == '\\' && quote != '`' {
            if i + 1 < s.len() && s[i + 1] == '\n' {
                *line += 1;
            }
            i += 2;
            continue;
        }
        if c == quote {
            if i + 1 < s.len() && s[i + 1] == quote {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    // A backslash as the last character steps past the end.
    i.min(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(script: &str) -> Vec<(String, usize)> {
        split(script).into_iter().map(|s| (s.text, s.line)).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(texts("SELECT 1; SELECT 2"), vec![("SELECT 1".into(), 1), ("SELECT 2".into(), 1)]);
        assert_eq!(texts("SELECT ';'\n;\n\nSELECT 2;"), vec![("SELECT ';'".into(), 1), ("SELECT 2".into(), 4)]);
        assert_eq!(texts("-- only a comment\n# another\n;;"), vec![]);
        assert_eq!(texts("DELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; END$$\nDELIMITER ;\nCALL p();").len(), 2);
    }

    #[test]
    fn unterminated() {
        assert_eq!(texts("SELECT 'abc"), vec![("SELECT 'abc".into(), 1)]);
        assert_eq!(texts("SELECT 'abc\\"), vec![("SELECT 'abc\\".into(), 1)]);
        assert_eq!(texts("SELECT 1 /* open"), vec![("SELECT 1 /* open".into(), 1)]);
        assert_eq!(texts("SELECT 1 /"), vec![("SELECT 1 /".into(), 1)]);
    }
}
