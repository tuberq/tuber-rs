use std::collections::HashMap;

/// Parse simple YAML `key: value` lines into a map.
pub fn parse_yaml_map(yaml: &str) -> HashMap<&str, &str> {
    let mut map = HashMap::new();
    for line in yaml.lines() {
        if line.starts_with("---") {
            continue;
        }
        if let Some((key, val)) = line.split_once(": ") {
            map.insert(key.trim(), val.trim().trim_matches('"'));
        }
    }
    map
}

/// Parse simple YAML list (`- item`) lines.
pub fn parse_yaml_list(yaml: &str) -> Vec<String> {
    yaml.lines()
        .filter_map(|line| line.strip_prefix("- ").map(|s| s.trim().to_string()))
        .collect()
}

/// Get a string value from the map.
pub fn get_str<'a>(map: &HashMap<&str, &'a str>, key: &str) -> String {
    map.get(key).unwrap_or(&"").to_string()
}

/// Get a u64 value from the map, defaulting to 0.
pub fn get_u64(map: &HashMap<&str, &str>, key: &str) -> u64 {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Get an f64 value from the map, defaulting to 0.0.
pub fn get_f64(map: &HashMap<&str, &str>, key: &str) -> f64 {
    map.get(key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// Get a bool value from the map (checks for "true").
pub fn get_bool(map: &HashMap<&str, &str>, key: &str) -> bool {
    map.get(key).map(|v| *v == "true").unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_yaml_map() {
        let yaml = "---\nversion: 0.2.11\nuptime: 12345\ncurrent-jobs-ready: 42\n";
        let map = parse_yaml_map(yaml);
        assert_eq!(map.get("version"), Some(&"0.2.11"));
        assert_eq!(map.get("uptime"), Some(&"12345"));
        assert_eq!(map.get("current-jobs-ready"), Some(&"42"));
    }

    #[test]
    fn test_parse_yaml_list() {
        let yaml = "---\n- default\n- emails\n- webhooks\n";
        let list = parse_yaml_list(yaml);
        assert_eq!(list, vec!["default", "emails", "webhooks"]);
    }

    #[test]
    fn test_get_u64() {
        let yaml = "count: 42\nbad: notanumber\n";
        let map = parse_yaml_map(yaml);
        assert_eq!(get_u64(&map, "count"), 42);
        assert_eq!(get_u64(&map, "bad"), 0);
        assert_eq!(get_u64(&map, "missing"), 0);
    }
}
