//! Changes single keys of a TOML document in place, keeping comments and
//! formatting, on top of toml_edit.

use anyhow::{Result, bail};
use toml_edit::{DocumentMut, Item, Table, TableLike, Value};

pub struct Doc(DocumentMut);

impl Doc {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(Doc(text.parse()?))
    }

    /// Sets the key at `path`. An existing value keeps its surrounding
    /// whitespace and trailing comment; a missing table is created as a
    /// standard `[table]`.
    pub fn set(&mut self, path: &[&str], mut value: Value) -> Result<()> {
        let (leaf, parents) = path.split_last().expect("empty key path");
        let table = descend(self.0.as_table_mut(), parents, true)?.expect("created");
        match table.get_mut(leaf) {
            Some(Item::Value(old)) => {
                *value.decor_mut() = old.decor().clone();
                *old = value;
            }
            Some(_) => bail!("cannot set {}: it is a table", path.join(".")),
            None => {
                table.insert(leaf, Item::Value(value));
            }
        }
        Ok(())
    }

    /// Removes the key at `path`; reports whether it existed.
    pub fn unset(&mut self, path: &[&str]) -> Result<bool> {
        let (leaf, parents) = path.split_last().expect("empty key path");
        Ok(match descend(self.0.as_table_mut(), parents, false)? {
            Some(table) => table.remove(leaf).is_some(),
            None => false,
        })
    }
}

impl std::fmt::Display for Doc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

fn descend<'a>(
    mut table: &'a mut dyn TableLike,
    keys: &[&str],
    create: bool,
) -> Result<Option<&'a mut dyn TableLike>> {
    for (i, key) in keys.iter().enumerate() {
        if table.get(key).is_none() {
            if !create {
                return Ok(None);
            }
            let mut t = Table::new();
            t.set_implicit(false);
            table.insert(key, Item::Table(t));
        }
        table = match table.get_mut(key).and_then(Item::as_table_like_mut) {
            Some(t) => t,
            None => bail!("{} is not a table", keys[..=i].join(".")),
        };
    }
    Ok(Some(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv
cwd = "~/work/t3"
autostart = true             # login
restart = "always"           # policy

[env]
PORT = "3773"  # avoid 3000
DEBUG = "1"
"#;

    // Requirement: Edits keep comments
    #[test]
    fn set_replaces_values_and_keeps_comments() {
        let mut d = Doc::parse(SAMPLE).unwrap();
        d.set(&["env", "PORT"], "4000".into()).unwrap();
        d.set(&["restart"], "on-failure".into()).unwrap();
        assert_eq!(
            d.to_string(),
            SAMPLE
                .replace(r#""3773""#, r#""4000""#)
                .replace(r#""always""#, r#""on-failure""#)
        );
    }

    // Requirement: Edits keep comments
    #[test]
    fn set_inserts_into_existing_and_new_tables() {
        let mut d = Doc::parse(SAMPLE).unwrap();
        d.set(&["env", "NEW"], "x".into()).unwrap();
        d.set(
            &["env_file"],
            [".env"].into_iter().collect::<toml_edit::Array>().into(),
        )
        .unwrap();
        d.set(&["log", "keep"], 5.into()).unwrap();
        let out = d.to_string();
        assert!(
            out.contains("restart = \"always\"           # policy\nenv_file = [\".env\"]\n"),
            "{out}"
        );
        assert!(out.contains("DEBUG = \"1\"\nNEW = \"x\"\n"), "{out}");
        assert!(out.ends_with("\n[log]\nkeep = 5\n"), "{out}");
        assert!(out.starts_with("# ~/.config/hum/services/t3.toml\n"));
    }

    // Requirement: Edits keep comments
    #[test]
    fn unset_removes_only_that_key() {
        let mut d = Doc::parse(SAMPLE).unwrap();
        assert!(d.unset(&["env", "DEBUG"]).unwrap());
        assert!(!d.unset(&["env", "MISSING"]).unwrap());
        assert!(!d.unset(&["log", "keep"]).unwrap());
        assert!(d.unset(&["autostart"]).unwrap());
        assert_eq!(
            d.to_string(),
            SAMPLE
                .replace("DEBUG = \"1\"\n", "")
                .replace("autostart = true             # login\n", "")
        );
    }

    #[test]
    fn multiline_array_and_dotted_keys() {
        let mut d = Doc::parse(
            "command = [\n  \"t3\", # program\n  \"server]\",\n]  # trailing\nenv.A = \"1\"\n",
        )
        .unwrap();
        d.set(
            &["command"],
            ["a"].into_iter().collect::<toml_edit::Array>().into(),
        )
        .unwrap();
        d.set(&["env", "A"], "2".into()).unwrap();
        assert_eq!(
            d.to_string(),
            "command = [\"a\"]  # trailing\nenv.A = \"2\"\n"
        );
    }

    #[test]
    fn non_table_parent_is_refused() {
        let mut d = Doc::parse("env = \"x\"\n").unwrap();
        assert!(d.set(&["env", "B"], "2".into()).is_err());
    }
}
