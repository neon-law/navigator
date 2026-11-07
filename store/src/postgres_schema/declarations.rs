//! Join each CREATE TABLE declaration before extracting its type and references.

pub(super) fn lines(sql: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut in_table = false;
    let mut pending = String::new();
    let mut depth = 0usize;
    let mut quoted = false;
    for raw in sql.lines() {
        let line = raw.trim();
        if !in_table {
            output.push(line.to_string());
            in_table = line.starts_with("CREATE TABLE ");
            continue;
        }
        if line == ");" && !quoted {
            if !pending.trim().is_empty() {
                output.push(pending.trim().to_string());
                pending.clear();
            }
            output.push(line.to_string());
            in_table = false;
            depth = 0;
            continue;
        }
        let mut chars = line.chars().peekable();
        while let Some(character) = chars.next() {
            if !quoted && character == '-' && chars.peek() == Some(&'-') {
                break;
            }
            match character {
                '\'' => quoted = !quoted,
                '(' if !quoted => depth += 1,
                ')' if !quoted => depth = depth.saturating_sub(1),
                ',' if !quoted && depth == 0 => {
                    output.push(pending.trim().to_string());
                    pending.clear();
                    continue;
                }
                _ => {}
            }
            pending.push(character);
        }
        if !pending.is_empty() {
            pending.push(' ');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::lines;

    #[test]
    fn multiline_checks_and_foreign_keys_stay_in_one_declaration() {
        let actual = lines(concat!(
            "CREATE TABLE item (\n",
            "    id UUID PRIMARY KEY,\n",
            "    project_id UUID\n        REFERENCES project(id),\n",
            "    CONSTRAINT state CHECK (\n        status IN ('open', 'closed')\n    )\n",
            ");\n",
            "ALTER TABLE item ADD COLUMN note TEXT;\n",
        ));
        assert_eq!(
            actual,
            [
                "CREATE TABLE item (",
                "id UUID PRIMARY KEY",
                "project_id UUID REFERENCES project(id)",
                "CONSTRAINT state CHECK ( status IN ('open', 'closed') )",
                ");",
                "ALTER TABLE item ADD COLUMN note TEXT;",
            ]
        );
    }

    #[test]
    fn quoted_commas_parentheses_and_comments_preserve_the_next_column() {
        let actual = lines(concat!(
            "CREATE TABLE item (\n",
            "    note TEXT DEFAULT 'a,b)it''s', -- a comment,\n",
            "    id UUID PRIMARY KEY\n",
            ");\n",
        ));
        assert_eq!(actual[1], "note TEXT DEFAULT 'a,b)it''s'");
        assert_eq!(actual[2], "id UUID PRIMARY KEY");
    }
}
