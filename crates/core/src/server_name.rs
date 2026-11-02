/// A server name is one URL path segment in `/mcp/<name>`.
/// The rule: 1 to 63 characters, lowercase letters, digits, and `-`, and it starts with a letter or digit.
pub fn is_valid_server_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= 63
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_names() {
        assert!(is_valid_server_name("petstore-prod"));
        assert!(is_valid_server_name("a"));
        assert!(is_valid_server_name("1api"));
    }

    #[test]
    fn rejects_bad_names() {
        assert!(!is_valid_server_name(""));
        assert!(!is_valid_server_name("-petstore"));
        assert!(!is_valid_server_name("Petstore"));
        assert!(!is_valid_server_name("pet/store"));
        assert!(!is_valid_server_name("pet store"));
        assert!(!is_valid_server_name(&"a".repeat(64)));
    }
}
