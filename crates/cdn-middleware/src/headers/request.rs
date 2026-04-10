use crate::headers::variables::VariableContext;
use cdn_common::{HeaderAction, HeaderRule};

/// Apply custom request header rules to an upstream request.
///
/// Actions:
/// - set: set/overwrite the header
/// - add: only add if the header doesn't exist
/// - remove: remove the header
/// - append: append value (comma-separated)
///
/// Returns a list of (action, name, resolved_value) tuples to apply.
/// The caller (proxy) applies them to the actual RequestHeader.
pub fn apply_request_rules(rules: &[HeaderRule], vars: &VariableContext) -> Vec<HeaderOp> {
    rules
        .iter()
        .map(|rule| {
            let value = rule.value.as_deref().map(|v| vars.substitute(v));
            HeaderOp {
                action: rule.action.clone(),
                name: rule.name.clone(),
                value,
            }
        })
        .collect()
}

/// A resolved header operation ready to apply.
#[derive(Debug, Clone)]
pub struct HeaderOp {
    pub action: HeaderAction,
    pub name: String,
    pub value: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(action: HeaderAction, name: &str, value: Option<&str>) -> HeaderRule {
        HeaderRule {
            action,
            name: name.to_string(),
            value: value.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_set_rule() {
        let rules = vec![rule(HeaderAction::Set, "X-Custom", Some("value1"))];
        let vars = VariableContext::new();
        let ops = apply_request_rules(&rules, &vars);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0].action, HeaderAction::Set));
        assert_eq!(ops[0].name, "X-Custom");
        assert_eq!(ops[0].value.as_deref(), Some("value1"));
    }

    #[test]
    fn test_variable_substitution() {
        let rules = vec![rule(HeaderAction::Set, "X-Site", Some("${site_id}"))];
        let mut vars = VariableContext::new();
        vars.set("site_id", "my-site".to_string());
        let ops = apply_request_rules(&rules, &vars);
        assert_eq!(ops[0].value.as_deref(), Some("my-site"));
    }

    #[test]
    fn test_remove_rule() {
        let rules = vec![rule(HeaderAction::Remove, "X-Internal", None)];
        let vars = VariableContext::new();
        let ops = apply_request_rules(&rules, &vars);
        assert!(matches!(ops[0].action, HeaderAction::Remove));
        assert!(ops[0].value.is_none());
    }

    #[test]
    fn test_multiple_rules() {
        let rules = vec![
            rule(HeaderAction::Set, "X-A", Some("1")),
            rule(HeaderAction::Remove, "X-B", None),
            rule(HeaderAction::Add, "X-C", Some("3")),
        ];
        let vars = VariableContext::new();
        let ops = apply_request_rules(&rules, &vars);
        assert_eq!(ops.len(), 3);
    }
}
