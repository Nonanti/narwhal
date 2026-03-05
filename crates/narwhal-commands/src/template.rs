//! Built-in SQL templates (v1.3 #10).
//!
//! A tiny library of common statement shapes the user can drop into
//! the editor with `:tpl <name>`. The expansion is intentionally
//! plain text \u2014 no tab-stops, no cursor parking. That ships fast
//! and gives users 80 % of the value: the boilerplate is gone, the
//! placeholders show *what* needs filling in, and `Ctrl-N` / `f`
//! help with the rest.
//!
//! Tab-stop driven insertion (`DataGrip` / `IntelliJ` live-templates)
//! is a v2 concern; it requires teaching the vim machine about
//! parking-cursor stops.

/// All built-in templates by name. Order is the discovery order
/// `:tpl ?` lists them in (alphabetical for stability).
pub const BUILTINS: &[(&str, &str)] = &[
    (
        "del",
        "DELETE FROM $table$\nWHERE $condition$;\n",
    ),
    (
        "ins",
        "INSERT INTO $table$ ($columns$)\nVALUES ($values$);\n",
    ),
    (
        "join",
        "SELECT $cols$\nFROM $left$ AS l\nINNER JOIN $right$ AS r ON l.$lkey$ = r.$rkey$\nWHERE $condition$;\n",
    ),
    (
        "sel",
        "SELECT $cols$\nFROM $table$\nWHERE $condition$;\n",
    ),
    (
        "upd",
        "UPDATE $table$\nSET $column$ = $value$\nWHERE $condition$;\n",
    ),
    (
        "with",
        "WITH cte AS (\n    SELECT $cols$ FROM $table$\n)\nSELECT * FROM cte;\n",
    ),
];

/// Look up a built-in template by name. Returns the unexpanded body
/// string when found. Names are case-sensitive (always lowercase in
/// practice) to keep the lookup O(n) without a normalisation pass.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, body)| *body)
}

/// List every built-in by name. Used by `:tpl` with no argument to
/// produce a discovery hint.
#[must_use]
pub fn list() -> Vec<&'static str> {
    BUILTINS.iter().map(|(n, _)| *n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_template_known() {
        assert!(lookup("sel").is_some());
        assert!(lookup("sel").unwrap().contains("SELECT"));
        assert!(lookup("sel").unwrap().contains("$cols$"));
    }

    #[test]
    fn unknown_template_returns_none() {
        assert!(lookup("nosuch").is_none());
    }

    #[test]
    fn list_contains_known_names() {
        let names = list();
        assert!(names.contains(&"sel"));
        assert!(names.contains(&"ins"));
        assert!(names.contains(&"upd"));
        assert!(names.contains(&"del"));
        assert!(names.contains(&"join"));
        assert!(names.contains(&"with"));
    }

    #[test]
    fn placeholders_use_dollar_dollar_syntax() {
        // The convention is enforced by tests so a future contributor
        // doesn't sneak in a `${name}` style that breaks the surface
        // area users learn.
        for (_, body) in BUILTINS {
            let dollar_count = body.matches('$').count();
            assert!(dollar_count > 0, "template should have placeholders");
            // Even number → balanced `$name$` pairs.
            assert_eq!(dollar_count % 2, 0, "unbalanced placeholders in: {body}");
        }
    }
}
